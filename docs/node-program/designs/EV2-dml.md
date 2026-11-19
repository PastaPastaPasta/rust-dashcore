# EV2: the deterministic masternode list (`dash-evo::dml`), reconciled design

| | |
|---|---|
| Status | D2 **reconciled** design, **rev4** after the r4 GPT check (`reviews/EV2-design-r4-gpt.md`, APPROVE-WITH-CHANGES, minors only) and the manager's rulings; rev3 followed the r3 closure check, rev2 the r2 review, rev1 the r1 reviews (`reviews/EV2-design-r1-opus.md` APPROVE-WITH-CHANGES, `reviews/EV2-design-r1-gpt.md` REWORK) and the manager's rulings; dispositions for every finding are in the last section. Merged from `EV2-dml-fable.md` and `EV2-dml-opus.md`; the Reconciliation log lists each disagreement with its Core evidence |
| Date | 2026-10-08 |
| Author | EV2 reconciler (Fable 5.1), over the Fable 5.1 and Opus 5.5 legs |
| Task | EV2a: `MnList` data model (imbl, unique properties, payee-queue structure), diff/snapshot codecs with the canonical `EvoSnapshot` MN-list encoding, Core's iteration order, `EvoStore` plus an in-memory store. EV2b: `build_new_list_from_block` in Core's order, PoSe, payee selection (queue plus reference twin), confirmations, collateral spends including ordinary transactions, the DML part of `CDeterministicMNManager::{ProcessBlock, UndoBlock}` |
| Binding inputs | A1 §2.5, §2.9, §2.10.1–2, §2.25, §3.5, §3.6, §3.9–3.11, §4.4, §4.6, Appendix A; PLAN §0 (D2), §4.4 (EV0–EV4, LQ1a/b), §8.2; PRD FR-EVO-01…05, FR-EVO-12, FR-STO-08, NFR-PERF-01…04, NFR-CEQ-12; O17 `O17-pin-delta/evo.md` EVO-08, -10, -11, -13, -15, -25, -28, -29 and `O17-release-profile.md` §4.1; R4 §1.15 quirks 13, 17, 23, 24, 25, 26, 28; R4-D §3; CS0 §4.5, §8.4; DEC-11, DEC-16, DEC-24 D3, DEC-30 N3/N6, DEC-37, DEC-41 |
| Core verified at | `v23.1.8` (728f505583) and `v24.0.0-rc.3` (0364ed626e) in `~/workspace/dash`, read with `git show <tag>:<path>`. Line refs are written `v23:<n>` / `v24:<n>`, file named once per paragraph; a single number means both tags agree. The vendored immer is identical at both tags (`git diff --stat v23.1.8 v24.0.0-rc.3 -- src/immer` is empty) |
| Evidence | `/work/scratch/ev2-opus/` (Opus leg: `scripts/champ_order.py`, testnet data at height 1,133,744; `bench-imbl/`), `/work/scratch/ev2-rec/` (this reconciliation: `champ_check.py`, `mainnet-check.log`, `testnet-check.log`) |

> **EMBARGO-1 / EMBARGO-2.** Nothing here relates to either; neither leg looked for them.

## Changes in rev4

1. **R4-m1 and the no-panic ruling.** `MnList` gains `Provenance { FromGenesis, Decoded }`. I-P is asserted only on `FromGenesis` lists; on `Decoded` lists and their build results `check_invariants()` compares the lazily computed P1 with a literal `ForEachMNShared` count and never asserts P1 true (the A/B/C fixture is a successful Core build with three valid MNs at `lastPaid == 101`). **No invariant check may panic or abort on any path reachable from decoded or imported data**; violations there are values or typed errors; hard assertions only in test/xcheck builds or on proven provenance (§2.4, §3.4, §7). The old "built lists satisfy both by construction" wording is marked superseded (Changes in rev2 item 2, R2-F2 disposition).
2. **R4-m2.** The P1-completeness claim is the precise implication `list.height > 0 ∧ lastPaid == list.height ⇒ pay_height ≥ list.height` (a valid MN with `lastPaid 0, registered −2` has pay height −2 and is simply not counted). The height-0 future-*positive*-registration fixture is relabelled as a fast-path case (`E E E R E`, P1 ∧ P2 true) and a separate negative-registration MN gives the P2-fallback fixture (§3.4, §6.2).
3. **R4-n1.** The twin's sharing contract now lists `scan.rs` alongside the payload decoder and canonical encoder; the independent check of the literal scans is the direct Core differential coverage of fallback inputs, not a second Rust scan (§3.4).

## Changes in rev3

1. **Continuation candidate search (GPT R3-F1, major; ruling 1).** The fast path walks the unbounded range `pay_height ≥ list.height` of `payee_queue` and applies Core's exact predicates (`lastPaid == height`, valid, Evo, `consecutive < 4`); P1 gives uniqueness, P2 keeps the candidate in range. The "equality-height run" wording is gone from §2.4 and §3.4 (a future `nPoSeRevivedHeight`, which the reader allows, puts the candidate above the list height: fixture `E E E R E`). Future-revival, height-0/future-registration and read → build fixtures added to the corpus (§6.2); the old Opus m1 disposition is reconciled. The canonical reader is unchanged.
2. **No builder certification (GPT R3-F2, major; ruling 2).** `fast_payee_ok` is always computed from the resulting list itself: P2 is `height > 0` (else an O(n) pass), P1 is a count over the same `payee_queue.range(height..)` walk filtered by `lastPaid == height`, provably complete when `height > 0` and equivalent to tracking the count during the build (§3.4, §2.4). The `rebuild_probe` fixture (A/B Evo at `lastPaid 101`, C regular; expect Core's `A A A A A A A A B`) is in §6.2.
3. **R3-m1.** The CHAMP phase-start rule is restricted to confirmations, decay and the streak reset; QcTx applies punishments in member-vector order (reverse subsequence inside a bucket), payee bookkeeping precedes the streak snapshot, each phase snapshots afresh (§2.7, §3.3); fixture `quirk_41_qctx_order`.
4. **R3-m2.** `scan.rs` holds the production `payee_scan`/`projected_scan`, compiled unconditionally; only the twin (`reference.rs`) is behind `xcheck`; a default-feature build is an EV2b-1 gate (§2.1, §7).
5. **R3-m3.** The counter text now states Core's retained-counter / per-addition `max(id + 1, counter)` rule (`ApplyDiff`/`AddMN`, v23:483-485 / v24:513-515) and points to §2.8 (§2.8, §6.2).

## Changes in rev2

1. **Bulk updates applied in Core's order (GPT R2-F1, blocker; ruling 1).** Core snapshots every bulk phase in CHAMP order before mutating (confirmations iterate `prevList`, `DecreaseScores` collects `toDecrease` first, the streak reset iterates `newList2`), so Core's sequence is the phase-start `core_order()` filtered to the eligible MNs. EV2b finds candidates through the indexes and applies them in that order; only within-bucket order matters, so the rule reduces to applying colliding candidates in the phase-start bucket sequence (§2.7, §3.2 rows 1, 2, 6). The twin applies every mutation literally; I-C compares buckets, `core_order()` and operator first matches after every block, eviction and reconstruction; targeted fixture `quirk_41_bulk_decay` (§6.2). The "independent updates" claim is withdrawn.
2. **Payee and projection on any reader-accepted list (GPT R2-F2, major; ruling 2).** The indexed path is gated on two checked preconditions, P1 (I-P) and P2 (`height > 0` or no negative registration), kept in `fast_payee_ok`; ~~built lists satisfy them by construction~~ (superseded in rev3/rev4: always computed from the list itself, never asserted on decoded provenance), other lists check them lazily in O(n); otherwise Core's literal `payee_scan`/`projected_scan` run (compiled in every build). `GetProjectedMNPayees`' first-scan accumulation of every candidate's remaining payments is now described; both reviewer fixtures are in the corpus (§2.4, §3.4, §6.2). The reader is not made stricter.
3. **Minors.** R2-m1: `UniqueMap(imbl::HashMap<UniqueKey, UniqueSlot>)` (imbl 7.0.2's alias supplies `std::collections::hash_map::RandomState` and `DefaultSharedPtr`). R2-m2: round-trip assertions state their equality projection per layer (raw Core codecs: canonical content / `IsEqual`; our records: full equality incl. counter, ids, buckets). Trailer rule: a bucket is captured when its *sequence* changed, including order-only changes; `apply_record` clears the order and payee caches (§2.8, §2.9, §6.2).

## Changes in rev1

1. **Collision-node order reproduced (GPT F1, blocker; Opus n1).** Inside an immer collision node the order is mutation history, most recently inserted-or-replaced first, and every `UpdateMN` (hot-only ones included) moves the MN to the front; verified against the vendored immer at both pins (§2.7). `MnList` gains `collision_buckets`, maintained at the choke point and carried in our record trailers, so `core_order()`, `GetMNByOperatorKey`'s first match, `BuildSimplifiedDiff`, `protx list` and `masternodelist` reproduce the **from-genesis online 64-bit** order, which is the parity target. Recorded Core inconsistencies: 32-bit hosts (7-level trie), reconstructed lists, imported datadirs (§3.11). New invariant I-C; `oracle_mnlist_core_order` now takes an operation sequence.
2. **Registration counter in the diff record (GPT F2, major).** Core's diff cannot restore `nTotalRegisteredCount` after a register-and-spend; our `mn_diff` envelope carries `total_registered_after` (and the bucket trailer), so reconstruction equals the online list byte for byte (§2.8–2.10). Tests for one-block and adjacent-block register-and-spend, replacements, restart, reorg (§6.2). Oracle provenance caveat (§3.11 item 4, Q14).
3. **Keyed table hasher (GPT F3, major; Opus n6).** `UniqueMap` uses SipHash-1-3 with a per-process random key (`RandomState`); the pre-image key stays (DEC-50). PF0 `crit:mnlist_unique_probe` measures SipHash vs Fx vs `OrdMap` on normal keys and on the reviewer's collision fixture (§2.2, §5.5).
4. **Differential contract split (GPT F4, major; Opus m2).** Validated-block targets (`diff_dml_block`, `diff_dml_sequences`, recording the rejecting layer) are separated from raw-helper targets (`diff_dml_apply`, constrained to the reachable intersection); expected out-of-domain helper differences are tested as such; the EVO-15 vectors now name the layer and the correct strings; payee/projection oracles get deployment context and no domain restriction; the O2a-EV entry points are merge prerequisites (§3.10, §6.3, §7).
5. **Replay windows (Opus M1).** 1,028,160–1,128,159 (first non-null commitments at ≈ 1,082,963, PoSe, bans, a revive), 20k blocks from the 1,899,071 vector (evonode continuation), 2,118,528–2,138,895 (across MN_RR); recorded members from an O7-style replica, never the live oracles (§6.4, §7).
6. **Opus minors.** m1: `payee()`/`projected_payees()` return Core's last match in `core_order()` on any list; I-P is a post-build invariant only (§2.4, §3.4). m3: unique keys tested against Core's `GetUniquePropertyHash` vectors (`oracle_unique_property_hash`). m4: null-value rules per property (§2.2). m5/GPT F5: unvalidated hashing encoder vs validated storage encoder; 1,028,159 is hash/bytes-only (§2.9, §6.4). m6: the member-source trait is LQ1 rev1 §2.6's text, `MemberError` (§2.11).
7. **Nits.** n2 raw `nType` in the shared-collateral check (§3.5); n3 `get_by_operator_key` with a null/invalid query (§2.6); n4 step 9 reworded, cross-type removal noted (§3.2.1); n5 §3.9/§3.11 no longer claim v24 parity for DML store faults; n7 negative `nCount` is RP2a's. GPT F6: the benchmark is labelled a limited model, effective k corrected, the monotonic-gain sentence removed (§5.2–5.3).

---

## 0. Summary

1. **One immutable, persistent list.** `MnList` is a value type over `imbl` 7 persistent maps (O(1) clone, structural sharing), the role `immer::map` plays in Core (`deterministicmns.h` v23:135-143 / v24:136-144). Core's three maps are kept: MNs by proTxHash in memcmp order, the internal-id map, and Core's unique-property map with reference counts. Every mutation goes through `try_add` / `update` / `update_hot` / `remove`, Core's `AddMN`/`UpdateMN`/`RemoveMN` with the same failure order and all-or-nothing semantics, and ends in one choke point that maintains four secondary indexes and O(1) counts. Indexes are never serialized; a list has one content whatever path produced it.
2. **Core's iteration order is CHAMP order, not memcmp (A1 erratum; quirk 41).** `ForEachMN` walks `immer::map` with `ImmerHasher = ReadLE64(proTxHash)` and B = 5; inside a collision node the order is most-recently-set first. That order is observable in every `mnlistdiff`/`qrinfo` (`BuildSimplifiedDiff`), in `protx list`, `masternodelist` and `GetMNByOperatorKey`. The reconciler re-verified the trie order on the live mainnet oracle (4,753 MNs, a 454-entry added subset and 467 deletions) and testnet (542 MNs): §2.7. `MnList::core_order()` reproduces it, bucket order included (from-genesis online 64-bit history as the parity target); `iter()` stays memcmp for the canonical encoding and the SML root.
3. **Unique-property map keyed by the serialization pre-image** (Fable), with Core's reference counts (both), in a keyed-SipHash table (rev1). Core's key is `SHA256d(serialization)`; the map is never iterated or serialized, only probed and compared, so keying by the pre-image gives the same collision classes (quirk 23) and saves a SHA256d per probe; the keyed hasher denies cheap table collisions on attacker-chosen bytes. §2.2.
4. **Hot/cold `Dmn`** (Opus): a `Copy` hot part (payment heights, PoSe) and a shared `Arc<MnCold>`. PoSe decay and payee bookkeeping copy about 20 bytes, allocate no scripts, and do not invalidate the SML-hash cache, which lives on the cold part per validity. §2.3.
5. **Codecs.** `mn_snapshot` is v24's `SerializeCanonicalMNList` byte for byte; `CanonicalMNListHash = SHA256d(list)`. `mn_diff` is v24's `SerializeCanonicalMNListDiff`, which for every diff `BuildDiff` produces is byte-identical to v24's own `dmn_D4` row (`BuildDiff` sorts added MNs by internalId at both pins; v24's `dmn_D4` writer sorts updates). Both carry our envelope (DEC-11). Core's `dmn_S3`/`dmn_D4`/`dmn_D3` rows get decode-only readers for EV4. §2.9.
6. **`build_new_list_from_block` is a line-by-line port** of `RebuildListFromBlock` (`specialtxman.cpp` v23:185-520 / v24:309-795): same order, same reject strings, Core's throws mapped to `failed-procspectxsinblock`. It never reads the release profile: the one profile row (EVO-15) is dead code inside EV2b under `V24_0` because EV1 rejects first. §3.2, §3.10.
7. **`process_block` (EV2b) owns the DML half of `CDeterministicMNManager::ProcessBlock`**: diff, snapshot cadence (every 576 blocks plus the first DIP3 block), `Effect::ResetPlatformBan { pro_tx_hash, height }` emitted before the `bad-dip3-enf-block` check, as Core does at both pins. CH7a calls it only when `!just_check`. The QcTx branch takes **one `QuorumMemberSource<E>` trait owned by EV0 in LQ1's shape**, with an `EffectSink` so LQ1b's snapshot effects pass through, and `MemberError::Store` is a node error, never a block verdict (the path Core v24 takes for `EvoDbInconsistencyError`, `specialtxman.cpp:989-992`). Ownership table in §3.8.
8. **Performance.** Opus's model measured 1.44 M instructions/block for the indexed path against 7.6 M for a Core-shaped twin at mainnet scale (5.3×); the pre-image key removes a further SHA256d per probe. The PLAN gate (≥ 2× the twin) has margin; the floor is eager PoSe decay (Q5). §5.
9. **Verification.** L0 from `evo_deterministicmns_tests` (both pins) and `evo_snapshot_tests` (v24); model fuzzing against the twin; L1 split into validated-block targets (`diff_dml_block`, recording the rejecting layer) and raw-helper targets (`diff_dml_apply` on the reachable intersection), ≥ 24 CPU-h each, plus codec, diff, order and property-hash targets; L2 `CanonicalMNListHash` per block over mainnet 1,028,160–1,128,159, 20k blocks from 1,899,071 and 2,118,528–2,138,895, and at the A1 §4.6 boundaries; one hidden RPC (`getcanonicalmnlist`) and the "O2a-EV" entry points as merge prerequisites. §6.
10. **Quirks proposed:** 41 (CHAMP order) and 42 (in-block list exceptions and order-dependent strings). Fable's "26" is R4's existing quirk 26, not a proposal. §3.12.

---

## 1. Scope and non-goals

**EV2a (M2–M3, `dash-evo::dml`):**
- types: `Dmn`, `MnHot`, `MnCold`, `MnType`/`RawMnType`, `UniqueKey`, `PayeeKey`, `Counts`;
- `MnList` with Core's `AddMN`/`UpdateMN`/`RemoveMN` semantics (rollback, error classes, reference counts of the unique map; FR-EVO-01, quirk 23, the `AddMN` contract quirk 28 relies on);
- the secondary indexes and counts, with an invariant checker;
- the read API EV1, EV3, EV4, LQ1a/b, GV1, AM1, RP2a and P6b consume (§2.6);
- `core_order()` (quirk 41);
- `MnListDiff`/`MnStateDiff` (Core's field bitmask), `diff_to` (`BuildDiff`), `apply_diff` (`ApplyDiff`);
- codecs: canonical list, `CanonicalMNListHash`, canonical diff, `CDeterministicMN` and state at `SER_DISK`, the `dmn_S3`/`dmn_D4`/`dmn_D3` readers for EV4's importer (FR-EVO-12);
- `EvoStore`'s DML slice, `DmlDelta`, `MemEvoStore`, `ListCache` and `resolve_list` (Core `GetListForBlockInternal`; FR-STO-08);
- test kit: list and MN builders and a synthetic block generator (feature `test-utils`).

**EV2b (M3, `dash-evo::dml`):**
- `build_new_list_from_block` in Core's exact order with every reject string;
- PoSe (`pose_punish`, `decrease_scores`, `calc_max_pose_penalty`, `calc_penalty`);
- confirmations, payee bookkeeping, evonode streaks (FR-EVO-02/03);
- `payee` (`GetMNPayee`), `projected_payees` (`GetProjectedMNPayees`) and the reference twin (FR-EVO-04; quirk 13);
- collateral spends by any transaction (quirk 25);
- the V24 paths: types 11 and 12, `set_state_version`, cross-scheme probes, the shared-collateral spend rule (FR-EVO-05; EVO-08, -11, -13);
- `process_block` / `undo_block` (the DML part of `CDeterministicMNManager::{ProcessBlock, UndoBlock}`);
- `-consensusxcheck` wiring points; criterion/iai benches and PF0 workloads.

**Not in scope (owner):**
- `CheckProRegTx`/`CheckProUpServTx`/…, NetInfo validation, `MakeSignString`: **EV1**. EV2b trusts that every special tx passed `CheckSpecialTxInner` against `pprev` (`specialtxman.cpp` v24:871-875; v23 equivalent) and re-checks only what Core re-checks in-block.
- Payload and NetInfo types and decoding: **BS6a/BS6c**. EV2 consumes `BlockPayloads` decoded once by CH7a (§2.11).
- BLS lazy keys and re-encoding: **BS11a-t** (§3.6 lists the contract).
- `GetBlockTxOuts`, payee and value validation: **EV3**. EV2b hands EV3 the payee it computed.
- SML entries, `merkleRootMNList`, `mnlistdiff`, the evodb importer wiring: **EV4**. EV2a provides `iter()`, `core_order()`, the SML-hash slots and the readers.
- Member selection and rotation snapshots: **LQ1a/b**, through `QuorumMemberSource`; EV2a gives LQ1b `MnList::scratch()` + `try_add`.
- Orchestration in `process_special_txs_in_block`, `EvoDelta` in the B1 batch, `MnListChanged`, `EvoReadView`, the concurrent list cache: **CH7a**.
- Platform-ban state (`mncache`): **SP1/AM1** apply the effect.
- RPC JSON: **RP2a/RP2b** (EVO-28/29 are RPC rows).
- `ApplyDiffForSnapshot` and the `EvoSnapshot` v3 container (assumeutxo): out of scope (A1 NG-6). Only the MN-list and MN-list-diff codecs are implemented, because that is what `CanonicalMNListHash` and our storage need.

**PRD mapping.** FR-EVO-01 (EV2a), FR-EVO-02…04 (EV2b), FR-EVO-05 (EV2b phase 3), FR-STO-08 (EV2a store), FR-EVO-12 (readers, for EV4). Stories US-MN-10 (via RP2a), US-LCS-01/04/05 (via EV4/P6b), US-NODE-01.

---

## 2. Data model and public API

### 2.1 Module layout, dependencies, principles

```
dash-evo/src/dml/
  mod.rs        re-exports; ARCHITECTURE.md table of Core files mirrored
  types.rs      MnType, RawMnType, Dmn, MnHot, MnCold, PayeeKey, Counts
  unique.rs     UniqueKey (pre-image bytes), UniqueMap
  list.rs       MnList: storage, try_add / update / update_hot / remove, reads
  index.rs      the four secondary indexes and counts; check_invariants()
  order.rs      core_order(): immer's CHAMP iteration order (quirk 41)
  payee.rs      pay_height, payee(), projected_payees()
  pose.rs       calc_max_pose_penalty, calc_penalty, pose_punish, decrease_scores
  diff.rs       MnListDiff, MnStateDiff, diff_to (BuildDiff), apply_diff (ApplyDiff)
  codec.rs      canonical list / diff / Dmn / state codecs, canonical_mn_list_hash; core_disk::{s3, d4, d3}
  store.rs      EvoStore DML slice, DmlDelta, MemEvoStore, ListCache, resolve_list
  build.rs      build_new_list_from_block, set_state_version, apply_pro_up_serv (EV2b)
  process.rs    process_block, undo_block (EV2b)
  scan.rs       Core's literal payee_scan / projected_scan over core_order(): the production fallback (§3.4), compiled in every build
  reference.rs  the twin: RefMnList and build_new_list_from_block_ref   [cfg(any(test, feature = "xcheck"))]; reuses scan.rs
  testkit.rs    builders and a synthetic block generator                 [feature = "test-utils"]
```

- **Pure and sans-io.** Depends on `dash-consensus` (CS0: `CoreWrite`, `HashWriter`, `Block`, `Tx`, `BlockId`, `ChainAncestry`, `DeploymentView`, `BlockValidationError`, `UngatedRules`), CS5a's `CoinsView`/`Coin`, `dash-primitives` (BS6a/c payloads, `NetInfo`, `OutPoint`, `Script`, `KeyId`, `PlatformNodeId`), `dash-pkc` (BS11a-t `OperatorKey`), `imbl = "=7.0.2"` (exact pin, Q6). No I/O, clocks, globals. Core's `debugLogs` lines are `tracing` events with Core's text (§3.9); they never affect results.
- **Determinism (DEC-30 N6).** No `std` `HashMap`/`HashSet`. The unique map is an `imbl::HashMap<K, V>` (imbl 7.0.2's alias for `GenericHashMap<K, V, std::collections::hash_map::RandomState, DefaultSharedPtr>`), i.e. a **keyed SipHash hasher seeded per process**, behind a newtype that exposes no iteration (§2.2), which is what A1 §2.5 sketched ("lookups only, never iterated"): because nothing ever observes the map's layout, the random seed is unobservable and the determinism rule is met. Everything else is `imbl::OrdMap`/`OrdSet` or `Vec`.
- **Immutable entries.** `Dmn` and `MnCold` are never mutated in place; a change installs a new `Arc<Dmn>` (and a new `Arc<MnCold>` only when a cold field changed). Unchanged MNs keep their `Arc`s across blocks, which makes `diff_to` O(k) and SML-hash caching free.
- **Core's integer types.** `MnCold::version: i32`, heights `i32` (Core `int`, `dmnstate.h` v24:41-48), sentinels −1/0 as in Core, so the canonical bytes are identical. `MnList::height: i32` (−1 for the pre-DIP3 default list; A1 erratum, it wrote `u32`).

### 2.2 Keys and the unique-property map

- **`ProTxHash` order** is `uint256::operator<` = memcmp of the internal (little-endian) bytes (`CompareByLastPaid`'s tie-break, `deterministicmns.cpp` v23:168-181 / v24:143-156; the canonical sort, `snapshot.h` v24:244). `ProTxHash: Ord` is lexicographic over the **internal** bytes; a unit test pins it against Core-formatted hex, because display hex is byte-reversed (the reconciler's oracle run confirms `protx list` is neither memcmp nor display-hex order, `/work/scratch/ev2-rec/*.log`).
- **Core's key** is `GetUniquePropertyHash(v) = SerializeHash(v, SER_GETHASH, PROTOCOL_VERSION [| ADDRV2_FORMAT if CService is not AddrV1-compatible])` (`deterministicmns.h` v23:488-510 / v24:526-548). The map `mnUniquePropertyMap` is only ever **probed** (`count`, `find`, `set`, `erase`: v24:443, 448, 558-585), **compared** (`IsEqual` v24:492, `operator==` v24:623) and **snapshotted for rollback** (`deterministicmns.cpp` v24:452, 525, 621); it is never iterated, serialized or exposed (`git grep mnUniquePropertyMap` at both tags: only `deterministicmns.{h,cpp}`, a comment in `dmnstate.h`, and a test comment). Therefore any injective function of the serialization gives the same semantics; the identity (the bytes themselves) is the cheapest.

```rust
/// Core's GetUniquePropertyHash *pre-image*: the SER_GETHASH serialization of the value at
/// PROTOCOL_VERSION (+ ADDRV2_FORMAT for a CService that is not AddrV1-compatible). Two values occupy
/// the same slot in Core iff their serializations are equal (SHA256d of equal bytes; a SHA256d collision
/// between unequal pre-images is a hash break, out of the threat model), so keying by the bytes
/// preserves every collision class of quirk 23 without a SHA256d per lookup.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct UniqueKey(SmallVec<[u8; 48]>);   // 48 covers every property except DomainPort (V24, up to 256 bytes)
impl UniqueKey {
    pub fn collateral(op: &OutPoint) -> Self;            // txid[32] ‖ u32 n                           (36 bytes)
    pub fn service(s: &Service) -> Self;                 // ADDRv1 ip[16] ‖ port BE16 (18) or ADDRv2 net ‖ CompactSize ‖ addr ‖ port BE16
    pub fn domain(d: &DomainPort) -> Self;               // LIMITED_STRING(addr, 253) ‖ port BE16      (netinfo.h v24:176-180)
    pub fn key_id(k: &KeyId) -> Self;                    // [20]; owner key, or each share owner key
    pub fn platform_node_id(p: &PlatformNodeId) -> Self; // [20]: equal bytes ⇒ same slot as key_id (quirk 23)
    pub fn operator_key(k: &OperatorKey) -> Self;        // the 48 bytes in the key's own encoding (legacy iff state nVersion == 1)
}
pub struct UniqueSlot { pub owner: ProTxHash, pub refs: u32 }
/// imbl::HashMap behind a newtype: get/insert/remove/len/eq only, no iteration API. imbl 7.0.2's `HashMap<K, V>` is
/// `GenericHashMap<K, V, std::collections::hash_map::RandomState, DefaultSharedPtr>` (rev2, GPT R2-m1), i.e. keyed
/// SipHash-1-3 with a per-process random key: the key bytes are attacker-chosen (service addresses, keys, outpoints), and a
/// fixed hasher such as FxHash admits cheap exact-collision families (rev1, GPT F3: 512 valid IPv6 services with one Fx
/// hash). Equality is content equality (imbl's `test_eq` probes with the other map's hasher), so a random seed changes
/// nothing observable, including equality of a decoded list against an online one.
#[derive(Clone, PartialEq, Eq)]
pub struct UniqueMap(imbl::HashMap<UniqueKey, UniqueSlot>);
```

| Property | Serialized bytes | Added by `try_add` when (`deterministicmns.cpp` v24:454-507; v23:435-473) |
|---|---|---|
| collateral | `txid[32] ‖ u32 n` | always |
| each NetInfo entry | CService as above; DomainPort as above | per entry; an entry that is neither AddrPort nor DomainPort throws "invalid address" |
| owner | `keyIDOwner[20]`, or every `share.keyIDOwner[20]` for a shared MN | always (a null key fails `AddUniqueProperty` → throw) |
| operator | the 48 key bytes in the stored encoding | key not null |
| platformNodeID | `[20]` | Evo and non-null |

Cross-type collisions are reproduced, not fixed (quirk 23): `CKeyID` and `uint160` are both 20 raw bytes; a CJDNS ADDRv2 service is also 20 bytes (`06 10 ‖ ip[16] ‖ port`); TORv3/I2P ADDRv2 services are 36 bytes like an outpoint; legacy- and basic-encoded copies of one BLS point are distinct 48-byte strings.

**Null values (rev1, Opus m4).** `AddUniqueProperty` and `DeleteUniqueProperty` compare against `T{}` and return **false** for a null value (v24:552-555, 572-575); `UpdateUniqueProperty` returns early when old equals new, then skips a null `old` (no delete) and a null `new` (no add) (v24:604-612). The callers skip only the operator key and the platformNodeID when null (v24:497, 505). Hence, per property: a null collateral outpoint, a `CService{}`/`DomainPort{}` netinfo entry, or a null non-shared owner (or share owner) key makes `try_add` fail with Core's "duplicate …" text; `remove` of an MN holding such a value fails with Core's "Can't delete" / "internal error" text; `update` treats a null side as absent. Valid blocks cannot reach these (EV1 validates services and keys), but `fuzz_apply_diff`, `mnlist_model` and the L1 synthetic lists can; the twin and a `quirk_23_null_*` test pin them.

`refs` matters: `AddUniqueProperty` on a slot already owned by the **same** proTxHash increments the count (v24:559-566); `DeleteUniqueProperty` erases at 1, else decrements (v24:577-586). A reference above 1 arises only when one MN legitimately holds the same bytes twice (e.g. a shared MN whose share owner key equals another of its properties).

**Why not the hash (Opus), why not an `OrdMap` (Opus), and why a keyed table hasher (GPT F3).** The SHA256d key buys nothing observable and costs one SHA256d (≈ 3 compressions, ≈ 3–4.5 k instructions on agentbox, no SHA-NI) per `HasUniqueProperty`, `GetMNByCollateral`, and per input of every transaction in the spend pass; the pre-image key keeps Core's collision classes and drops that cost (DEC-50). What the pre-image does **not** keep is the computational behaviour of a cryptographic table key: with a fixed FxHasher over attacker-chosen bytes, the GPT reviewer built 512 distinct, Core-valid IPv6 `CService` pre-images with one Fx hash (`/work/scratch/ev2-rev-gpt/fx-probe`), making probes linear and insertions quadratic in the bucket. `UniqueMap` therefore uses keyed SipHash-1-3 with a per-process random key. An `OrdMap<UniqueKey>` (log n ≈ 14 compares of up to 48 bytes, four B-tree node copies of ≈ 1.4 KB per mutation) has bounded comparison cost but is slower to mutate than the HAMT (three or four 32-slot pointer nodes); PF0 `crit:mnlist_unique_probe` measures SipHash vs Fx vs OrdMap on normal keys **and on the collision fixture**, and the swap is one line (DEC-50(3)). Equivalence with Core is tested against Core's own hashes (rev1, Opus m3): `SHA256d(UniqueKey::x(v).bytes()) == GetUniquePropertyHash(v)` for every property type and class (IPv4, IPv6, CJDNS, TORv3, I2P, DomainPort, keyID, uint160, legacy and basic BLS), with vectors from `oracle_unique_property_hash` (§6.3), and `oracle_mnlist_diff`/`diff_dml_apply` compare whole lists so any slot divergence shows.

### 2.3 `Dmn`: hot and cold

Core's `CDeterministicMN` (`deterministicmns.h:43-94`) holds a `shared_ptr<const CDeterministicMNState>`; every update copies the whole state (`dmnstate.h` v23:33-155 / v24:33-175, about 400 bytes plus heap scripts and a lazy BLS wrapper). Per block almost every update is PoSe decay or payee bookkeeping, which touch five `int`s. The Rust split follows that:

```rust
pub struct Dmn {
    pub pro_tx_hash: ProTxHash,
    pub internal_id: u64,                 // VARINT on disk; consensus-deterministic from genesis (snapshot.h v24:229-236)
    pub collateral: OutPoint,
    pub operator_reward: u16,             // immutable after registration
    pub mn_type: MnType,                  // Regular = 0, Evo = 1 (u16 on the wire); weight 1/4, collateral 1000/4000 DASH (dmn_types.h v24:30-43)
    pub hot: MnHot,
    pub cold: Arc<MnCold>,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MnHot {                         // Core `int` fields
    pub last_paid_height: i32,            // nLastPaidHeight      (0 = never)
    pub consecutive_payments: i32,        // nConsecutivePayments
    pub pose_penalty: i32,                // nPoSePenalty
    pub pose_revived_height: i32,         // nPoSeRevivedHeight   (-1 = never)
    pose_ban_height: i32,                 // nPoSeBanHeight       (-1 = valid); private as in Core (dmnstate.h v24:36, 146-149)
}
impl MnHot { pub fn is_banned(&self) -> bool; pub fn banned_height(&self) -> i32;
             pub fn ban_if_not_banned(&mut self, h: i32); pub fn revive(&mut self, h: i32); }   // dmnstate.h v24:136-155

pub struct MnCold {
    pub version: i32,                     // nVersion (Core int, 4 bytes on disk); legacy key encoding iff == 1
    pub registered_height: i32,
    pub revocation_reason: u16,
    pub confirmed_hash: Hash256,
    pub confirmed_hash_with_pro_tx: Hash256,   // single SHA256(proTxHash ‖ confirmedHash), dmnstate.h v24:162-169
    pub key_id_owner: KeyId,                   // null for shared MNs (V24)
    pub pub_key_operator: OperatorKey,         // BS11a-t; CBLSLazyPublicKey semantics (§3.6)
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
    derived: ColdDerived,                      // never serialized, never compared
}
struct ColdDerived {
    unique_keys: OnceLock<UniqueKeys>,          // the UniqueKey bytes this cold state holds (built once; no hashing)
    sml_hash: [OnceLock<Hash256>; 2],           // EV4's SML entry hash, indexed by is_valid
}
impl MnCold {
    pub fn from_pro_reg(tx: &ProRegTx, registered_height: i32) -> Self;   // dmnstate.h v24:74-89
    pub fn reset_operator_fields(&mut self);   // v24:127-135: version = 1, key null, net_info = MnNetInfo::empty(), op payout empty, reason 0, platformNodeID 0; ports NOT reset
    pub fn update_confirmed_hash(&mut self, pro_tx: &ProTxHash, confirmed: &Hash256);   // v24:162-169
    pub fn is_shared(&self) -> bool;           // !shares.is_empty()
}
```

- **Which fields are hot:** exactly those the per-block steps change without touching keys or addresses: payee bookkeeping (`last_paid`, `consecutive`), `DecreaseScores` (`penalty`), `PoSePunish`/`BanIfNotBanned`/`Revive` (`penalty`, `ban`, `revived`). A ban through ProUpRegTx/ProUpRevTx also changes cold fields and builds a new cold anyway. Core's codec writes the hot fields contiguously (`dmnstate.h` v24:95-101), so the state encoder interleaves nothing unusual.
- **SML hash cache keyed by validity.** The SML entry depends on cold fields, `nType`, the proTxHash and `isValid = !banned` (`deterministicmns.cpp` v24:43-50). A ban or revive selects the other slot; decay never rehashes. EV4 fills the slots; EV2a owns them. This replaces Fable's per-`Dmn` `OnceLock` cleared on every update.
- **Equality** is per field in Core's member order (§3.7); `Dmn: PartialEq` short-circuits on `Arc::ptr_eq(cold)`.
- **A payload's `nType`** is `RawMnType(u16)` compared numerically with the MN's type (EVO-15 needs out-of-range values); `MnType` exists only for stored MNs.

### 2.4 `MnList`, indexes, invariants

```rust
#[derive(Clone)]                                       // O(1): one Arc increment per map plus the cached order
pub struct MnList {
    block_hash: BlockHash,                             // null while building (Core SetBlockHash(uint256()))
    height: i32,                                       // -1 for the pre-DIP3 default list
    total_registered: u32,                             // nTotalRegisteredCount
    // Core's three maps
    mns: OrdMap<ProTxHash, Arc<Dmn>>,                  // memcmp order: canonical encoding, SML root
    by_internal_id: OrdMap<u64, ProTxHash>,
    unique: UniqueMap,                                 // §2.2
    // secondary indexes (index.rs); pure functions of `mns`; never serialized
    payee_queue: OrdSet<PayeeKey>,                     // valid MNs, CompareByLastPaid order
    decaying: OrdSet<ProTxHash>,                       // valid ∧ pose_penalty > 0          (DecreaseScores)
    unconfirmed: OrdSet<(i32, ProTxHash)>,             // confirmed_hash null, keyed by registered height (confirmations)
    evo_streak: OrdSet<ProTxHash>,                     // Evo ∧ consecutive_payments != 0   (streak reset)
    counts: Counts,                                    // GetCounts() in O(1)
    collision_buckets: OrdMap<u64, Arc<VecDeque<ProTxHash>>>,   // immer collision-node sequences for shared 64-bit hashes (§2.7); empty on real networks
    core_order: OnceLock<Arc<[ProTxHash]>>,            // derived from mns + collision_buckets (§2.7); reset by every mutation
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PayeeKey { pub height: i32, pub pro_tx: ProTxHash }   // (CompareByLastPaid_GetHeight, memcmp)
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts { pub total_mn: u32, pub total_evo: u32, pub valid_mn: u32, pub valid_evo: u32 }
impl Counts { pub fn total(&self) -> u32; pub fn enabled(&self) -> u32;
              pub fn total_weighted(&self) -> u32; pub fn valid_weighted(&self) -> u32 }   // Evo weight 4 (deterministicmns.h v24:269-294)
```

**Why four indexes and not five or six.** The two legs differed by two structures, both dropped here:
- Opus's **typed collateral mirror** `by_collateral: OrdMap<OutPoint, ProTxHash>` existed to skip the SHA256d of `GetMNByCollateral`. With the pre-image key, `unique.get(UniqueKey::collateral(prevout))` *is* Core's lookup, with Core's collision behaviour, at one hash-map probe; the mirror would add a path copy per registration/removal for nothing.
- Fable's **`last_payee` slot** (the MN with `last_paid_height == height`) served the evonode-continuation rule. It is derivable from `payee_queue`: when `list.height > 0`, every MN with `last_paid_height == height` has `pay_height ≥ height` (`pay_height` is `last_paid` or something larger, the `registered` substitution applying only to `last_paid == 0`), so it lies in `payee_queue.range(height..)`; on a list built from blocks that range is the paid MN plus same-height registrations and revivals (a revival or registration height cannot exceed the list height there), and on a reader-accepted list it may also hold future heights, which the filter of §3.4 discards. Scanning that range (§3.4) costs O(r log n) with r tiny on real lists and needs no height-dependent index (rev3: the earlier "equals the list height" wording was wrong for reader-accepted lists, GPT R3-F1).

**Invariants** (`check_invariants()` in debug, fuzz and xcheck builds; Fable's `debug_assert_indexes` and Opus's I-U/I-X/I-P merged). **Provenance rule (rev4, manager ruling with R4-m1):** `MnList` carries `provenance: Provenance { FromGenesis, Decoded }`, set to `FromGenesis` only by `default_pre_dip3()` and by a build whose predecessor is `FromGenesis`; decoding, `apply_diff`/`apply_record`, `try_add` by the importer or a test, and every build from a `Decoded` predecessor give `Decoded`. **No invariant check may panic or abort on any path reachable from decoded or imported data** (snapshots, evodb imports, peer-supplied diffs, oracle fixtures): on a `Decoded` list `check_invariants()` returns `Result<(), InvariantViolation>` and the caller treats a violation as a value or a typed error. Hard assertions (`assert!`, abort) are allowed only in test/xcheck builds, or on `FromGenesis` lists, whose temporal premises (`last_paid ≤ height`, `registered ≥ 0`, revival ≤ height) are proven by construction. I-P is the invariant this rule exists for: a reader-accepted predecessor can carry future `lastPaid` values, so a *successful* build from it can leave several valid MNs at `lastPaid == height` (the A/B/C fixture of §6.2), and asserting I-P there would fail on a list Core accepts and builds.
- **I-U.** Every MN's `UniqueKeys` map to it with the right `refs`, and every slot is owned by an MN in `mns`. Hence `get_by_collateral(op)` never dangles (Core would dereference null).
- **I-X.** Each index equals its definition recomputed from `mns`; `counts` equals a recount.
- **I-P.** At most one MN has `last_paid_height == height`, **on `FromGenesis` lists**. Proof: starting from the empty default list, `nLastPaidHeight` is written only by payee bookkeeping (one MN per block, value = the block height) or copied from older states, and registrations start at 0, so every stored `lastPaid` is at most the list height and a new block's height is fresh. Core's canonical reader enforces neither I-P nor the `lastPaid ≤ height` premise (rev1, Opus m1 / GPT F4; rev4, R4-m1), so a decoded list may break I-P **and so may a successful build from it** (`specialtxman.cpp` v23:486-515 / v24:759-790 keep foreign future `lastPaid` values and only reset their streaks). Therefore: on `FromGenesis` lists I-P is asserted (test/xcheck) after each build; on `Decoded` lists and their build results `check_invariants()` **never asserts P1 true**; it recomputes P1 by a literal `ForEachMNShared` count and compares it with the lazily computed `fast_payee_ok` inputs (§3.4), reporting a disagreement as `InvariantViolation`. `payee()`/`projected_payees()` give Core's answer either way.
- **I-C.** `collision_buckets` holds exactly the 64-bit hashes shared by two or more MNs, each bucket's member set is the keys with that hash, and the sequence follows §2.7's prepend / move-to-front / delete rules for the operation history since the list was built or decoded, with bulk phases applied in Core's phase-start order (rev2). Under xcheck the fast list's buckets are compared with the twin's after every block, and `core_order()` and `get_by_operator_key` first matches are compared after every block build, cache eviction and record reconstruction (not only after primitive operations).
- **P1 / P2** (`fast_payee_ok`, §3.4): computed from the list itself, never inherited from a predecessor or certified by the builder (rev3, GPT R3-F2); a list violating either uses the literal payee/projection scan.

### 2.5 Mutations: Core's `AddMN` / `UpdateMN` / `RemoveMN`, exactly

These are the semantics LQ1b's quirk 28 swallows, so the exact failure conditions are part of the contract.

```rust
impl MnList {
    /// CDeterministicMNList(): the pre-DIP3 default (hash 0, height -1, count 0); also LQ1b's MnsUsedAtH scratch list.
    pub fn default_pre_dip3() -> MnList;   pub fn scratch() -> MnList { Self::default_pre_dip3() }
    pub fn new(block_hash: BlockHash, height: i32, total_registered: u32) -> MnList;   // CDeterministicMNList(hash, h, n)
    pub fn with_block_hash(self, h: BlockHash) -> Self;  pub fn with_height(self, h: i32) -> Self;  // SetHeight asserts >= 0

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

- **`try_add` order and failures** (v24:442-507): (1) duplicate proTxHash; (2) duplicate internalId; (3) collateral; (4) each NetInfo entry in `GetEntries()` order (AddrPort or DomainPort; neither → `InvalidAddress`); (5) owner: every share key if shared, else `keyIDOwner` (a null key fails); (6) operator, if not null; (7) platformNodeID, if Evo and not null. Then `mns` and `by_internal_id`; if `bump_total`, `total_registered = max(id + 1, total)` truncated to `u32` as Core assigns the `uint64` to a `uint32_t` (v24:513-515). "Duplicate" means the slot exists **and is owned by another proTxHash**; the same owner increments `refs`.
- **`update`** (v24:527-582): (1) NetInfo: if the objects differ (deep compare, `shared_ptr_not_equal`), delete every old entry then add every new one ("internal error", "invalid address", "duplicate (…)" become variants); (2) owner key via `UpdateUniqueProperty`; (3) operator key; (4) platformNodeID, only for Evo. `UpdateUniqueProperty` returns early when old equals new; **v24** compares BLS keys by serialized hash (`deterministicmns.h` v24:596-600), **v23** by `operator==`, i.e. by point (v23:552-555). They differ only when a key changes encoding but not point, which needs V24's `SetStateVersion`; pre-V24 a state's encoding is a function of `nVersion`, which changes only together with the key (§3.7). We implement v24's rule: `UniqueKey::operator_key` compares bytes. Owner keys of shared MNs are immutable after registration.
- **`remove`** (v24:612-679) deletes in `try_add`'s order; a slot with `refs == 1` is erased, else decremented. `total_registered` is never decremented.
- **Rollback.** Core snapshots `mnUniquePropertyMap` before each primitive and restores it on failure (v24:452, 525, 621). With persistent maps the Rust primitives compute new maps into locals and commit all of them at the end, so a failing primitive leaves `self` untouched.
- **One choke point.** The four methods end in `IndexDelta::apply(old: Option<&Dmn>, new: Option<&Dmn>)`, which updates the four indexes and `counts` from the before/after entry and clears `core_order`. `PayeeKey` uses `pay_height` (§3.4).
- **Errors.** `MnListError` has one variant per Core `runtime_error` text in the four functions plus `PoSePunish`; `core_message()` renders Core's text for the `FAILURE!` log line (`specialtxman.cpp` v23:702 / v24:994). Inside `build_new_list_from_block` every `MnListError` becomes `DmlBuildError::Exception` → `failed-procspectxsinblock` (§3.9).
- **Unique keys are built once** per cold state (`ColdDerived::unique_keys`): no hashing, just the serialization bytes. Core recomputes `GetUniquePropertyHash` on every `Add`/`Update`/`Remove`, and v24's `UpdateUniqueProperty` hashes both BLS keys even when nothing changed.

### 2.6 Read API

```rust
impl MnList {
    pub fn block_hash(&self) -> &BlockHash;  pub fn height(&self) -> i32;  pub fn total_registered(&self) -> u32;
    pub fn counts(&self) -> Counts;  pub fn len(&self) -> usize;  pub fn is_default(&self) -> bool;
    pub fn get(&self, h: &ProTxHash) -> Option<&Arc<Dmn>>;                      // GetMN
    pub fn get_valid(&self, h: &ProTxHash) -> Option<&Arc<Dmn>>;                // GetValidMN
    pub fn is_valid(&self, h: &ProTxHash) -> bool;  pub fn is_pose_banned(&self, h: &ProTxHash) -> bool;
    pub fn get_by_internal_id(&self, id: u64) -> Option<&Arc<Dmn>>;
    pub fn get_by_collateral(&self, op: &OutPoint) -> Option<&Arc<Dmn>>;        // GetMNByCollateral: owner of the slot, Core semantics
    pub fn get_valid_by_collateral(&self, op: &OutPoint) -> Option<&Arc<Dmn>>;
    pub fn get_by_service(&self, s: &Service) -> Option<&Arc<Dmn>>;
    pub fn get_by_operator_key(&self, k: &BlsPublicKey) -> Option<&Arc<Dmn>>;   // GetMNByOperatorKey: first point-match in core_order() incl. bucket order (§2.7); an invalid or null query matches the first MN with a null operator key, as Core's Get() yields one invalidObj for both (bls.h:492-512); AM1 guards the query (Opus n3)
    pub fn has_unique(&self, k: &UniqueKey) -> bool;                            // HasUniqueProperty
    pub fn unique_owner(&self, k: &UniqueKey) -> Option<&Arc<Dmn>>;             // GetUniquePropertyMN
    pub fn has_operator_key_under_any_scheme(&self, k: &BlsPublicKey, except: &ProTxHash) -> bool;  // v24 deterministicmns.h:471-482 (EVO-13)
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Dmn>>;                      // memcmp order (canonical encoding, SML root)
    pub fn iter_valid(&self) -> impl Iterator<Item = &Arc<Dmn>>;
    pub fn core_order(&self) -> Arc<[ProTxHash]>;                               // immer CHAMP order (§2.7)
    pub fn payee(&self, r: PayeeRules) -> Option<&Arc<Dmn>>;                    // GetMNPayee (§3.4)
    pub fn projected_payees(&self, r: PayeeRules, n: usize) -> Vec<Arc<Dmn>>;   // GetProjectedMNPayees; Core's `nCount < 0 → {}` (v24:198-200) is RP2a's argument check (Opus n7)
    pub fn calc_max_pose_penalty(&self) -> i32;  pub fn calc_penalty(&self, percent: i32) -> i32;
    pub fn diff_to(&self, to: &MnList) -> MnListDiff;                           // BuildDiff (§2.8)
    pub fn is_equal(&self, rhs: &MnList) -> bool;   // Core IsEqual (v24 deterministicmns.h:487-525): ignores internal ids and total_registered
}
pub struct PayeeRules { pub v19: bool, pub mn_rr: bool }   // DeploymentActiveAfter(pindexPrev, V19 / MN_RR)
```

`get_by_collateral` keeps Core's unique-map semantics (the ProRegTx replacement step uses it without an equality check, §3.2.1); the spend pass adds Core's `collateralOutpoint == prevout` check (§3.5).

### 2.7 Core's iteration order (quirk 41)

Core's MN map is `immer::map<uint256, CDeterministicMNCPtr, ImmerHasher>` with `ImmerHasher(h) = ReadLE64(h.begin())` (`deterministicmns.h` v23:135-143 / v24:136-144) and immer's `default_bits = 5` (`src/immer/immer/config.hpp:126`, identical at both tags). `ForEachMN`/`ForEachMNShared` (v24:301-324; v23:300-323) are plain range-for loops over that map, so they visit MNs in CHAMP pre-order (`src/immer/immer/detail/hamts/champ_iterator.hpp`: a node's inline values first, then its children left to right, `step_down`/`step_right`):
- the slot at depth L is `(h >> 5L) & 31` (depth 12 has 4 bits);
- at each node, keys alone in their slot come first in slot order, then the sub-tries in slot order;
- keys with identical 64-bit hashes sit in a collision node at depth 13 (`max_shift`), **most recently inserted or replaced first** (rev1; see below).

**The trie shape is canonical; the order inside a collision node is not.** immer's erase re-inlines a one-entry subtree into its parent (`champ.hpp` `do_sub`, `sub_result::singleton`, v24 lines 1199-1240), so above depth 13 the order is a function of the key set alone. Inside a collision node the order is mutation history (verified against the vendored immer at both tags, identical sources):
- `do_add` on a collision node (`champ.hpp:565-576`): a key not present goes through `copy_collision_insert`, which **prepends** (`node.hpp:384-405`: the new value at index 0, the old sequence after it); a key already present goes through `copy_collision_replace`, which **moves it to the front** (`node.hpp:484-514`: new value, then the values before its old position, then those after).
- `do_sub` (`champ.hpp:1199-1210`): with more than two entries `copy_collision_remove` keeps the remaining sequence; with two, the survivor is re-inlined.
- Every `UpdateMN` is an `mnMap.set` of an existing key (`deterministicmns.cpp` v24:588; v23:557), so a payment, a confirmation or a PoSe decay moves that MN to the front of its bucket **without changing the key set**. The GPT reviewer's probe against both tags shows the sequence (`/work/scratch/ev2-rev-gpt/champ_probe.cpp`): insert 1,2,3,4 → `4 3 2 1`; replace 2 → `2 4 3 1`; erase 3 → `2 4 1`; re-add 3 → `3 2 4 1`.
- `hash_t = std::size_t` (`bits.hpp:23`), so on a **32-bit host** `ImmerHasher`'s `ReadLE64` is truncated to its low 32 bits and the trie is 7 deep (6 × 5 bits + 2); Core's release HOSTS include `arm-linux-gnueabihf`. Such a node orders its lists differently from every 64-bit node: a **Core inconsistency**, recorded, not emulated. The parity target is the 64-bit order.

**Both legs agreed on the trie algorithm; they disagreed on whether the order leaks.** It does:

| Site | v23.1.8 | v24.0.0-rc.3 | Consumer |
|---|---|---|---|
| `BuildSimplifiedDiff`: `mnList` in `to` order, `deletedMNs` in `from` order (mnlistdiff, qrinfo bytes) | `smldiff.cpp:122-146` | `:121-145` | EV4, P6b; dash-spv and the desktop as peers |
| `protx list` (`wallet`, `valid`, `registered`, `evo`) | `rpc/evo.cpp:1476, 1506` | `:1709, 1740` | RP2a, O14 |
| `masternodelist` | `rpc/masternode.cpp:612` | `:666` | RP2a |
| `GetMNByOperatorKey` (first point-match; two MNs can hold one point under two encodings pre-V24, and if their proTxHashes share the leading 64 bits the bucket order decides which MN is returned) | `deterministicmns.cpp:119-127` | `:93-102` | AM1, MP1a |
| `GetMNPayee` / `GetProjectedMNPayees` evonode scans (last match) | `:196-204` | `:171-179` | unique by I-P on lists built from blocks; imported lists use the last match in `core_order()` (§3.4) |
| `CDeterministicMNList::Serialize` (`dmn_S3`) | `deterministicmns.h:234-255` | `:235-255` | import only (DEC-11); our reader is order-insensitive |

What does **not** depend on it for its *result*: `GetMNPayee`'s minimum (strict total order), `GetProjectedMNPayees` (sorted), `GetCounts`, `BuildDiff` (a set; added sorted by internalId, updated/removed sorted on write in v24), `to_sml` (sorted by `proRegTxHash.Compare`, `simplifiedmns.cpp` v24:73-79) and `SerializeCanonicalMNList` (sorted, `snapshot.h` v24:244). The confirmation, decay and streak loops produce the same *state* in any order, but **not the same bucket sequence** (rev2, GPT R2-F1): each `UpdateMN` they perform is an `mnMap.set`, so the order in which Core applies them is part of the online history (next paragraph).

**Application order of bulk updates (rev2, ruling R2-F1).** Core never mutates the map it is iterating: every bulk phase takes a **snapshot in CHAMP order at the start of the phase** and then applies its updates in that order to the live list. Confirmations iterate `prevList` while mutating `newList` (`specialtxman.cpp` v23:205-218 / v24:330-343); `DecreaseScores` first collects `toDecrease` by `ForEachMNShared` and then calls `PoSeDecrease` over the vector (`deterministicmns.cpp` v23:334-350 / v24:309-326); the streak reset iterates a copy `newList2` while mutating `newList` (`specialtxman.cpp` v23:502-515 / v24:777-790). So a mid-loop reorder never changes Core's traversal, and for **these three phases** Core's sequence is exactly "the phase-start `core_order()` filtered to the eligible MNs". EV2b therefore applies each of them as: *find the candidates through the index, then apply them in the phase-start list's `core_order()`*. **The rule covers only confirmations, decay and the streak reset** (rev3, GPT R3-m1). The QcTx branch is not a CHAMP scan: `HandleQuorumCommitment` walks `members[i]` in **member-vector order**, each `i` paired with `validMembers[i]` (`specialtxman.cpp` v23:159-174 / v24:283-298), and that vector comes from LQ1's member selection, not from the MN map; EV2b calls `pose_punish` in exactly that order (§3.3), so inside a collision bucket the punished members end up in the *reverse of their member-vector subsequence*, not in the bucket's phase-start order (fixture `quirk_41_qctx_order`: bucket `4 3 2 1`, failed members presented as `1 2 3 4` → Core leaves `4 3 2 1`; a phase-start reordering would give `1 2 3 4`). Payee bookkeeping is a single `update_hot` of the payee, applied **before** the streak reset takes its `newList2` snapshot (v24:759-778), so the payee sits at the front of its bucket when the reset phase starts. Each phase takes a **fresh** snapshot (confirmations: `prev_list`; decay: after confirmations; streak reset: after payment); the block-start sequence is never reused. Only the order **within a collision bucket** matters: updates to keys in different buckets (or in no bucket) commute, because the trie shape is a function of the key set and a `set` touches no bucket but its own key's. So the implementation groups the candidates by 64-bit hash, applies the groups whose hash has a `collision_buckets` entry in that bucket's **phase-start** sequence (the bucket as it stood when the phase began: confirmations use `prev_list`'s buckets, decay the post-confirmation list's, the streak reset the post-payment list's), and applies everything else in index order. On real networks no bucket exists and the rule costs nothing; the GPT reviewer's fixture (four colliding keys, all decaying: Core's phase-start bucket `4 3 2 1` becomes `1 2 3 4` after decay, while applying in proTxHash order gives `4 3 2 1`) is the targeted test `quirk_41_bulk_decay` (§6.2). **The reference twin applies every mutation literally as Core does** (iterate a CHAMP-ordered snapshot, `set` each), and `check_invariants()` compares the fast list's and the twin's `collision_buckets` after every block (I-C, xcheck).

**Algorithm (`order.rs`).** (1) Map each key to `p(h)`, the 64-bit value whose bits are the chunks `c0 … c11` (5 bits each) then `c12` (4 bits), most significant first; sorting by `(p(h), proTxHash)` lays the trie's leaves out left to right. (2) Recurse over contiguous runs: at depth L emit the keys alone in their slot (slot order), then recurse into each multi-key run (slot order). (3) At depth 13 (identical 64-bit hashes) emit the bucket in the order stored in `collision_buckets` (below). O(n log n), computed once per list and cached in `core_order`.

**Collision buckets (rev1, GPT F1).** `MnList` carries `collision_buckets: OrdMap<u64, Arc<VecDeque<ProTxHash>>>`: one entry per 64-bit hash value shared by two or more MNs, holding Core's bucket sequence, front first. The choke point maintains it with immer's rules:
- on `try_add(k)` and on every `update`/`update_hot(k)`: if the bucket exists, `k` moves to its front in place (`push_front`, after removing `k` on an update; amortised O(1) plus the O(log B) lookup, so a list of n keys sharing one hash is built in linear work). If it does not exist, the MNs sharing `k`'s hash are the keys of `mns` whose first 8 internal bytes equal `k`'s, i.e. one `mns.range()` probe (memcmp order makes them contiguous; O(log n), no extra index); with another key present the bucket becomes `[k, other]`;
- on `remove(k)`: delete `k` from its bucket; a bucket of one is dropped.

The parity target is the **from-genesis online history on a 64-bit host**: the sequence a Core node produces when every list is built by `RebuildListFromBlock` from the previous online list, which is what `build_new_list_from_block` reproduces step for step (`AddMN`, `UpdateMN` and `RemoveMN` in Core's order, including hot-only updates). Two more Core inconsistencies are recorded with the 32-bit one: a Core node that **reconstructs** a list (`GetListForBlockInternal`: `AddMN` in `dmn_S3` iteration order, which reverses buckets, then `ApplyDiff`'s removed/added/updated passes) and a node that **imported** a datadir have different bucket orders from the online node, and from each other. Our records therefore carry the bucket orders (§2.9 envelope trailer) so that *our* reconstruction equals *our* online history, and `check_invariants()` adds **I-C**: every bucket's member set equals the keys sharing its hash, and every shared hash has a bucket. On mainnet and testnet no two MNs share a 64-bit hash today (the reconciler's `champ_check.py` lists have none; the probability at 5,000 MNs is about 7 × 10⁻¹³), so the trailer is a `CompactSize(0)` in practice and the order rule is exercised by synthetic tests: a birthday search over ProRegTx txids costs about 2³² hashes, which is minutes offline, so the `quirk_41_collision_*` fixtures craft two or three colliding ProRegTxs and replay insert, hot update, cold update, replacement, removal and re-registration against `oracle_mnlist_core_order(ops)` (§6.3), which takes the **operation sequence**, not the bytes.

**Consumers of the bucket order:** `core_order()` (and through it `GetMNByOperatorKey`'s first match, `BuildSimplifiedDiff`, `protx list`, `masternodelist`), and the last-match payee scans on imported lists (§3.4).

**Evidence (trie order).** Opus: `scripts/champ_order.py` reproduces the testnet oracle's `protx list registered` (522), `protx diff 1 tip` (524) and a 13-entry `protx diff tip−2000 tip` at height 1,133,744. Reconciler (`/work/scratch/ev2-rec/champ_check.py`, 2026-10-08): **mainnet** `protx list registered false 1196000` (4,753 MNs) `match=True`, `protx diff 1 1196000` (4,753) `True`, `protx diff 1176000 1196000` added (454) in the `to` list's CHAMP order `True` and **`deletedMNs` (467) in the `from` list's CHAMP order `True`**, `masternodelist json` `True`; **testnet** at 1,476,900: 542 MNs `True`, full diff `True`, 1-entry diff `True`, `masternodelist` `True`. Neither list equals memcmp or display-hex order.

**A1 §2.5 erratum.** "O's sorted maps, so that iteration is in Core's order" is wrong; memcmp order is what the canonical encoding and the SML root sort into. EV4's byte-identity target (`mnlistdiff` vs `getmnlistd`) and RP2a/O14's `protx list` comparison depend on `core_order()`.

### 2.8 Diffs

```rust
pub struct MnListDiff {
    pub added: Vec<Arc<Dmn>>,              // ascending internal_id (BuildDiff sorts: deterministicmns.cpp v23:388-391 / v24:362-366)
    pub updated: Vec<(u64, MnStateDiff)>,  // ascending internal_id
    pub removed: Vec<u64>,                 // ascending (Core std::set)
}
pub struct MnStateDiff { pub fields: u32, values: MnStateFields }   // Core Field_* bits, nVersion-first layout (dmnstate.h v23:160-178 / v24:180-204)
impl MnStateDiff {
    /// CDeterministicMNStateDiff(a, b) (dmnstate.h v23:221-242 / v24:250-281): per member in Core's member order;
    /// netInfo by deep comparison; pubKeyOperator recorded when its point (§3.6 identity) or its encoding changes (§3.7 erratum, BL-67); VERSION forced when
    /// PUB_KEY_OPERATOR or NET_INFO is set (v24:276-280).
    pub fn between(a: &Dmn, b: &Dmn) -> MnStateDiff;
    pub fn apply(&self, hot: &MnHot, cold: &MnCold) -> (MnHot, Option<Arc<MnCold>>);   // ApplyToState (v24:316-323); cold rebuilt only if a cold bit is set
}
impl MnList {
    /// ApplyDiff (deterministicmns.cpp v23:396-418 / v24:371-393): removed, then added (bumping the total), then updated.
    /// Core's semantics exactly (used by the twin, the importer and the oracle comparisons).
    pub fn apply_diff(&mut self, block: BlockHash, height: i32, d: &MnListDiff) -> Result<(), MnListError>;
    /// apply_diff, then restore `total_registered` and the touched collision buckets from the record (rev1, §2.8–2.9), then
    /// clear the `core_order` and `fast_payee_ok` caches (rev2).
    pub fn apply_record(&mut self, block: BlockHash, r: &MnListDiffRecord) -> Result<(), MnListError>;
}
```

- **`diff_to`** walks `imbl::OrdMap::diff`, which skips shared subtrees (`advance_skipping_shared`), so the cost is O(changes · log n). Add/Remove items become `added`/`removed`; an Update item becomes `MnStateDiff::between`, kept only if `fields != 0`, which is Core's `BuildDiff` (pointer compare, then field by field; v24:336-369). Core iterates **every** MN of `to` with a lookup in `from`. The twin `reference::build_diff_scan` is Core's algorithm.
- **One bit layout for both profiles.** v23.1.8 already has the nVersion-first format (`Field_nVersion = 0x0001`, `dmnstate.h` v23:160; evodb key `dmn_D4` at `deterministicmns.cpp` v23:34 / v24:32); v24 adds bits `0x80000…0x400000` for V24-only fields, which never appear pre-V24. The legacy layout (`dmn_D3`, `CDeterministicMNStateDiffLegacy`, v24 `dmnstate.h:327-483`) is read only by the importer.
- **Order of `updated`.** v23 serializes `updatedMNs` in `std::unordered_map` order (`deterministicmns.h` v23:591-605); v24 sorts them (v24:639-659). Ours are always sorted. The order is visible only in Core's evodb bytes (import-only) and in `protx diff`/`listdiff` JSON, which O14 compares as a set (RP2a/O14).
- **Application order vs build order.** `apply_diff` uses Core's order (removed, added, updated ascending). Core applies `updatedMNs` in `unordered_map` order; for a per-block diff no order can conflict, because EV1 checks every unique property against the **pprev** list, so a valid block never moves a property between two surviving MNs; only a removal frees one. Multi-block diffs (`ApplyDiffForSnapshot`) are never built here.
- **The registration counter is not in Core's diff (rev1, GPT F2).** A block that registers an MN and spends its collateral (or replaces it) in the same block adds and removes the MN, so `nTotalRegisteredCount` advances but `BuildDiff` sees no surviving addition; `ApplyDiff` starts from the **predecessor's retained counter** and bumps it only for the diff's additions, each `AddMN` setting `counter = max(added internalId + 1, counter)` (v23:483-485 / v24:513-515, `u32` assignment); a hidden register-and-spend therefore leaves the counter where the predecessor had it (rev3, GPT R3-m3: not "`max(id)+1` over the survivors", which cannot even express a predecessor counter of 7 with no surviving MN). Core's reconstructed list therefore has a lower counter than its online list, its next registration reuses an internal id, and its canonical snapshot and `CanonicalMNListHash` differ from the online node's; Core tolerates this by excluding both fields from `IsEqual` (`deterministicmns.h` v24:485-511) while still writing them into the canonical bytes. The GPT reviewer's probe against the v24 libraries shows `online count=1 restored count=0 … canonical equal=0`. Our promise is stronger: a list has one content whatever path produced it. So **our `mn_diff` record carries the post-block `total_registered`** (envelope, §2.9), and `apply_diff` from a stored record sets `total_registered` to it after applying the diff; `MnListDiffRecord { height, total_registered_after, diff, collision_buckets_after }`. The in-memory `MnListDiff` and the canonical diff bytes stay Core's; the extra fields live in our envelope only. The parity target for `total_registered` and internal ids is the **from-genesis online history**, the same one the canonical snapshot comment calls consensus-deterministic (`snapshot.h` v24:229-236). Tests (§6.2): register-and-spend in one block, register at `h` and spend at `h+1`, two replacements between snapshots, a trailing registration that disappears while other additions survive; evict every cache, reconstruct every height, restart, disconnect and reconnect, register again; compare counters, ids, canonical bytes and the existing-row rewrite check.

### 2.9 Codecs and `CanonicalMNListHash`

The snapshot value **is** v24's `SerializeCanonicalMNList` (`snapshot.h` v24:237-247); the diff value **is** `SerializeCanonicalMNListDiff` (v24:304-328); `CanonicalMNListHash` is SHA256d of the list encoding (`snapshot.cpp` v24:150-155). Stream type `SER_DISK`, version `CLIENT_VERSION`, so `nType` is always written (`deterministicmns.h:69-84`).

```
list    = blockHash[32] ‖ i32 height ‖ u32 totalRegistered ‖ CompactSize(n) ‖ n × Dmn      (Dmn sorted by proTxHash, memcmp)
Dmn     = proTxHash[32] ‖ VARINT(internalId) ‖ txid[32] ‖ u32 n ‖ u16 operatorReward ‖ State ‖ u16 nType
State   = i32 nVersion ‖ i32 registered ‖ i32 lastPaid ‖ i32 consecutive ‖ i32 penalty ‖ i32 revived ‖ i32 banHeight
          ‖ u16 revocationReason ‖ confirmedHash[32] ‖ confirmedHashWithProRegTx[32] ‖ keyIDOwner[20]
          ‖ pubKeyOperator[48]            (legacy encoding iff nVersion == 1; a null key is 48 zero bytes; CBLSLazyPublicKeyVersionWrapper)
          ‖ keyIDVoting[20] ‖ NetInfoSerWrapper(netInfo, extended = nVersion >= 3)   (MnNetInfo = one CService, 18 zero bytes when empty)
          ‖ (nVersion >= 3 ? payouts ‖ shares ‖ u32 earlyPeriodBlocks ‖ i64 earlyPenalty : CScript scriptPayout)
          ‖ CScript scriptOperatorPayout ‖ platformNodeID[20] ‖ (nVersion < 3 ? u16 platformP2PPort ‖ u16 platformHTTPPort)
diff    = CompactSize(a) ‖ a × Dmn (sorted by (internalId, proTxHash)) ‖ CompactSize(u) ‖ u × (VARINT id ‖ StateDiff) ascending id
          ‖ CompactSize(r) ‖ r × VARINT id ascending
StateDiff = VARINT(fields) ‖ for each member in Core's *tuple* order whose bit is set: its encoding
hash    = SHA256d(list)
```

Sources: `dmnstate.h` v23:83-110 / v24:93-125 (state), v24:288-314 (state diff; note the tuple order is not the bit order: `payouts`, `shares`, `nEarlyPeriodBlocks`, `nEarlyPenalty` serialize between `scriptPayout` and `scriptOperatorPayout`; `pubKeyOperator` uses the wrapper with `state.nVersion == 1`, `netInfo` with `state.nVersion >= 3`; `PUB_KEY_OPERATOR`/`NET_INFO` set without `VERSION` is a decode error, v24:292-295); `snapshot.h` v24:237-394. VARINT is `VarIntMode::DEFAULT`.

**Why the canonical diff and not `dmn_D4` (the Fable/Opus disagreement collapses).** `BuildDiff` sorts `addedMNs` by `internalId` at **both** pins (v23:388-391 / v24:362-366); internal ids are unique within a list, so the canonical sort by `(internalId, proTxHash)` yields the same sequence. v24's `CDeterministicMNListDiff::Serialize` (`deterministicmns.h` v24:639-659) sorts `updatedMNs` and writes `removedMns` from a `std::set`. Hence for every diff Core builds, **v24's `dmn_D4` bytes equal the canonical diff bytes**; v23's `dmn_D4` differs only in the unordered `updatedMNs` (import-only). What the canonical reader adds is the *decoder*: strict order, `internalId` presence, payout and netinfo limits, `CheckCanonicalEncoding`, bounded counts (`snapshot.h` v24:331-387). We take the canonical encoding with the canonical reader. DEC-11 allows either ("own formats"); A1 §2.9 already names the canonical list for `mn_snapshot` and the CanonicalMNListHash acceptance target (PLAN EV2a) needs the list codec anyway, so one codec family, one reader discipline and one fuzz corpus serve storage, the hash, the O8 vectors and EV4's import of `EvoSnapshot` files.

**One encoding for both profiles.** v23.1.8 has no `snapshot.*`, but the `CDeterministicMN`/state/state-diff encodings it would feed are byte-identical for every pre-V24 state (the only differences are V24-gated branches), so a v23-profile node's lists hash exactly as the v24 oracle's over mainnet history (A1 §2.25 point 1). The pre-DIP3 default list hashes too (`GetHeightForSnapshotCodec` returns `nHeight`, −1: `deterministicmns.h` v24:340), which is why `height` is `i32`.

```rust
pub fn encode_canonical_list(list: &MnList, w: &mut impl CoreWrite);
pub fn decode_canonical_list(bytes: &[u8], lim: &SnapshotLimits) -> Result<MnList, CodecError>;
pub fn canonical_mn_list_hash(list: &MnList) -> Hash256;                 // HashWriter, no buffering
pub fn encode_canonical_diff(d: &MnListDiff, w: &mut impl CoreWrite);
pub fn decode_canonical_diff(bytes: &[u8], budget: &mut usize) -> Result<MnListDiff, CodecError>;
pub mod core_disk {                                                      // decode-only, for EV4's evodb importer
    pub fn decode_list_s3(bytes: &[u8]) -> Result<MnList, CodecError>;   // CDeterministicMNList::Unserialize: any (HAMT) order, try_add(bump=false) each
    pub fn decode_diff_d4(bytes: &[u8]) -> Result<MnListDiff, CodecError>;        // updated in any order (v23 rows)
    pub fn decode_diff_d3_legacy(bytes: &[u8]) -> Result<MnListDiff, CodecError>; // CDeterministicMNStateDiffLegacy bit mapping (v24 dmnstate.h:452-482)
}
```

- **Two encoders, one byte layout (rev1, Opus m5 / GPT F5).** `canonical_mn_list_hash(list)` and `encode_canonical_list_unvalidated` write Core's `SerializeCanonicalMNList` bytes for **any** list, the pre-DIP3 default list included (Core's serializer and `CanonicalMNListHash` do not validate: `snapshot.h` v24:237-247, `snapshot.cpp` v24:150-155; the default list encodes to 41 bytes). `encode_canonical_list` (the storage path) first applies `ValidateCanonicalMNInvariants` (`snapshot.cpp` v24:104-125), which rejects a negative height as Core's **reader** does (`snapshot.h` v24:256), so a stored `mn_snapshot` is always readable. Round-trip tests are scoped to the reader's accepted domain; pre-DIP3 fixtures compare hash and bytes only (§6.4).
- **Decoder rules from `UnserializeCanonicalMNList`** (v24:249-298), each with Core's error text: negative height; `n ≤ EVO_SNAPSHOT_MAX_MNS` (100,000); per entry a nested CompactSize budget of `EVO_SNAPSHOT_MAX_MN_COMPACT_ITEMS`; re-encoding equals the consumed bytes after materialising the lazy key (`CheckCanonicalEncoding`, v24:193-201, 158-162); `internalId` present; `payouts.len() ≤ 8`; non-empty NetInfo validates (BS6a `validate() == Success`); strictly increasing proTxHash; at most 8 consecutive entries sharing the leading 64 bits (quirk 41's collision class); every entry through `try_add(bump=false)`, errors becoming codec errors; `max(internalId) < totalRegistered` when `n > 0`. Our encoder applies `ValidateCanonicalMNInvariants` (`snapshot.cpp` v24:104-125) first, so we never write what Core refuses to read. The canonical diff reader (v24:331-387) is ported likewise.
- **Our envelope** (DEC-11; rev1): every `mn_snapshot`/`mn_diff` value is `u8 record_version = 1 ‖ u8 codec_id ‖ VARINT height ‖ body ‖ trailer`, `codec_id ∈ {CANONICAL_LIST, CANONICAL_DIFF}`. The **trailer** carries what Core's bytes cannot reconstruct: for a diff, `u32 total_registered_after` (§2.8) and `collision_buckets_after` (`CompactSize(b) ‖ b × (u64 hash ‖ CompactSize(m) ‖ m × proTxHash[32])`, every bucket whose sequence this block changed, **including buckets touched only by order** (an update whose state change cancelled, or a hot-only update `BuildDiff` dropped) — the choke point marks a bucket dirty on every `set`/`remove`, not on every diff item); for a snapshot, `collision_buckets` for the whole list (the canonical body already carries `totalRegistered`). On real networks both bucket lists are `CompactSize(0)`. No checksum (RocksDB block checksums). A later codec (compressed diff, lazy decay) adds a `codec_id`; unknown ids are refused with the version in the message (FR-STO-02). Core's raw codecs (`decode_canonical_*`, `core_disk::*`) never see the trailer; `store.rs` splits it off.
- **The bytes-as-a-type rule.** CS0 §4.5 seals `CoreBytes` to transactions, headers and blocks; it cannot carry DML records. The same guarantee is given another way: the encoder types of `dml::codec` are crate-private, so no stored DML byte can come from base-sdk or a hand-built buffer; stored records are wrapped in a `DmlRecordBytes` newtype that only `dml::codec` can mint. No RPC returns DML bytes; the P2P `mnlistdiff` bytes are EV4's. The DESIGNER brief's "`CoreBytes` for any stored bytes" and CS0's seal conflict here; Q9 asks CS0 to rule.

### 2.10 The store: `EvoStore` DML slice, deltas, reconstruction

```rust
/// A1 §2.5 EvoStore, DML slice (EV0 owns the trait; EV2a lands these items with MemEvoStore). Keys are block hashes;
/// nothing is ever erased. Store errors are fail-stop (A1 Appendix A erratum: Result, not Option).
pub trait DmlReader: Sync {
    fn mn_list_snapshot(&self, block: &BlockHash) -> Result<Option<Arc<MnList>>, StoreError>;
    fn mn_list_diff(&self, block: &BlockHash) -> Result<Option<Arc<MnListDiffRecord>>, StoreError>;
}
pub struct MnListDiffRecord { pub height: i32, pub total_registered_after: u32, pub diff: MnListDiff,
                              pub collision_buckets_after: Vec<(u64, Vec<ProTxHash>)> }   // rev1: §2.8, §2.9 trailer
pub trait EvoStore: DmlReader { /* best_block, credit_pool, mnhf_signals, mined_commitment(s), quorum_snapshot as in A1 */ }
/// Goes into EvoDelta (EV0) and so into the Durable B1 batch (A1 §3.5.4). `record` is what mn_diff stores (Core's diff
/// plus the trailer, §2.8–2.9); `snapshot` is encoded with its bucket trailer.
pub struct DmlDelta { pub block: BlockHash, pub height: i32, pub record: Arc<MnListDiffRecord>, pub snapshot: Option<Arc<MnList>> }

pub trait ListCache: Sync {
    fn get(&self, block: &BlockHash) -> Option<Arc<MnList>>;
    fn offer(&self, list: &Arc<MnList>, hint: CacheHint);             // Tip | QuorumBase | Mini | Built
}
pub struct MemEvoStore { /* BTreeMap snapshots/diffs; LruListCache; stubs for the EV3/LQ1b/LQ2 items */ }
impl MemEvoStore { pub fn apply(&mut self, d: &DmlDelta); pub fn undo(&mut self, at: &BlockHash); pub fn set_tip(&mut self, at: Option<BlockId>); }

/// Core GetListForBlockInternal (deterministicmns.cpp v23:778-870 / v24:826-939): the list *after* `at`.
pub fn resolve_list(at: BlockId, chain: &dyn ChainAncestry, deployments: &dyn DeploymentView, params: &DmlParams,
                    store: &dyn DmlReader, cache: &dyn ListCache) -> Result<Arc<MnList>, DmlStoreError>;
```

- **Semantics.** Below DIP3 (`active_at`) the answer is the default list (v24:830-832). Otherwise walk back through `prev`, stopping at a cached list or a stored snapshot and collecting diffs; a block at or above DIP3 with neither snapshot nor diff is Core's "initial snapshot" `CDeterministicMNList(hash(D−1), D−1, 0)` (v24:881-889), which in our store happens only at `DIP3Height − 1` (not DIP3-active: the walk stops with the default list; `apply_diff(D)` sets hash and height, so both yield the same list). Then apply the diff records forward (`apply_diff`, then `total_registered = record.total_registered_after` and the bucket trailer, §2.8–2.9), offering a mini-snapshot every 32 heights (v24:899-917; not during reindex) and the result with `Tip`/`QuorumBase` hints (v24:920-936). A reconstructed list is therefore byte-equal to the online list, counter, ids and `core_order()` included; `-consensusxcheck` asserts it whenever both exist (§6.4).
- **A missing diff for a DIP3-active block above the first is corruption**: `DmlStoreError::Inconsistent`, which CH7a turns into a fail-stop abort (A1 I5; Core's text "Found EvoDB inconsistency, you must reindex"). Core's other fabrication case (an evodb older than its chain) cannot arise: rdashd rebuilds imported Core datadirs (A1 §2.9).
- **Cache policy is not observable** (O17 EVO-25 `none`). `LruListCache` mirrors v24's shape: pin the tip, keep DKG-interval bases within `dkgInterval × (keepOldConnections + 1)`, mini-snapshots every 32 heights, cleanup when > 64 lists, drop lists older than `LIST_DIFFS_CACHE_SIZE` (`CleanupCache` v24:975-1014; constants `deterministicmns.h` v24:760-769 / v23:681-684). CH7a owns the concurrent implementation and its budget.
- **Snapshot cadence:** `height % 576 == 0` (`DISK_SNAPSHOT_PERIOD`) **or** the first DIP3 block. Core writes the latter only when `pindex->pprev == m_initial_snapshot_index` (v24:722; v23:690), a marker set by a reconstruction that happened to walk past DIP3; in a from-genesis sync it is unset, so Core usually does not write it. We always do: a storage choice (DEC-11) that bounds every reconstruction to 575 diffs.
- **`undo` keeps the diff.** Core's `UndoBlock` reads the diff to compute the inverse `MNListUpdates` and erases only caches (v24:784-817; v23:736-769); the row stays, so `mn_list(at)` for a disconnected block still resolves, which `protx diff` and `mnlistdiff` serving across a reorg rely on (A1 §2.9 "kept").

### 2.11 EV2b entry points

```rust
pub struct DmlParams {                                  // from ChainParams (BS5)
    pub min_confirmations: i32,                          // nMasternodeMinimumConfirmations: mainnet 15 (chainparams.cpp v24:177), testnet/devnet/regtest 1 (:386, :569, :810)
    pub dip3_height: i32, pub dip3_enforcement_height: i32, pub dip3_enforcement_hash: Option<BlockHash>,   // mainnet 1,028,160 / 1,047,200 / :187; testnet 7,000 / 7,300 / :396
    pub llmqs: &'static [LlmqParams],                    // every configured type (dkgInterval, keepOldConnections)
}
pub struct DmlRules<'a> {
    pub params: &'a DmlParams,
    pub v19: bool, pub mn_rr: bool,                      // DeploymentActiveAfter(pindexPrev, ·)
    pub v24: bool,                                       // SpecialTxRules.v24 = GetSpecialTxRules(pindexPrev) (validation.cpp v24:2659); v23 is_v24_deployed (specialtxman.cpp v23:224)
}
pub struct PrevBlock<'a> { pub id: BlockId, pub hash: BlockHash, pub height: i32, pub chain: &'a dyn ChainAncestry }

/// `view.GetBestBlock().IsNull()` in Core decides whether external collateral is re-checked (specialtxman.cpp v24:378-387).
pub enum CollateralView<'a> { Coins(&'a dyn CoinsView) /* connect, TestBlockValidity, miner */, Unchecked /* Core's null-best-block path: evodb repair, the L2 harness */ }

/// **One trait, owned by EV0; the text is LQ1 §2.6 (rev3)** (the reference; reproduced here, not redefined). Core
/// llmq::utils::GetAllQuorumMembers(type, base) (utils.cpp v23:569-659 / v24:634-730): members in Core's order; empty when the
/// type is not enabled at base.prev, base has no parent, base is not a valid quorum base for the type, or rotation yields none.
/// Any snapshot the computation stores is emitted to `effects` (A1 §2.10.1), also when the caller later fails. Each returned
/// entry carries the state of the list it was taken from (LQ1 provenance rule); EV2b reads `pro_tx_hash` only.
pub trait QuorumMemberSource {
    fn members(&self, t: LlmqType, base: BlockId, effects: &mut dyn EffectSink) -> Result<Arc<[Arc<Dmn>]>, MemberError>;
}
pub enum MemberError {
    /// A read failure, or a record missing although an established store invariant requires it (the MN list of a
    /// connected block). FAIL-STOP: a node error, never a block verdict and never failed-procspectxsinblock (Core v24
    /// specialtxman.cpp:989-992; LQ1 deviation D4). Normal absence is a value, not this error: an absent snapshot row
    /// (SnapshotRows::get → Ok(None); the rotation breaks, LQ1 28a/28e) and unavailable work-block data
    /// (CoinbaseFacts::BlockDataUnavailable → the hash-form modifier). The member computation reads no commitment.
    Store(StoreError),
    /// Core's IsQuorumTypeEnabled throws runtime_error for an unknown type; Core's catch maps it to failed-procspectxsinblock.
    /// Unreachable from EV2b: bad-qc-commitment-type is checked first (§3.3).
    UnknownLlmqType(u8),
}

pub struct BuiltList { pub list: MnList /* block_hash null */, pub payee: Option<Arc<Dmn>> }
pub enum DmlBuildError {
    Invalid(BlockValidationError),                       // Core's explicit state.Invalid(BLOCK_CONSENSUS, …)
    Exception(DmlException),                             // a Core std::runtime_error throw: CH7a maps it to failed-procspectxsinblock
    Store(StoreError),                                   // a local store fault: CH7a maps it to a node error (state.Error), never a block verdict
}
pub enum DmlException { List(MnListError), Members(u8) /* MemberError::UnknownLlmqType, unreachable */, OutOfDomain(&'static str) /* Core UB, unreachable after EV1/LQ2; never a panic */ }

/// Core CSpecialTxProcessor::RebuildListFromBlock (specialtxman.cpp v23:185-520 / v24:309-795).
pub fn build_new_list_from_block(
    block: &Block, payloads: &BlockPayloads, prev: PrevBlock<'_>, prev_list: &MnList,
    coins: CollateralView<'_>, members: &dyn QuorumMemberSource, rules: &DmlRules<'_>,
    effects: &mut dyn EffectSink, trace: &mut dyn DmlTrace,
) -> Result<BuiltList, DmlBuildError>;

/// The DML part of CDeterministicMNManager::ProcessBlock (deterministicmns.cpp v23:649-734 / v24:689-782).
/// Called by CH7a only when DIP3 is active at the block and fJustCheck is false (specialtxman.cpp v23:628 / v24:913).
pub fn process_block(block: &Block, payloads: &BlockPayloads, at: (BlockHash, i32), prev_list: &Arc<MnList>, new_list: MnList,
                     params: &DmlParams, is_first_dip3_block: bool, effects: &mut dyn EffectSink)
    -> Result<ProcessedList, BlockValidationError>;
pub struct ProcessedList { pub list: Arc<MnList>, pub diff: Arc<MnListDiff>, pub delta: DmlDelta, pub change: MnListChange }
pub struct MnListChange { pub old: Arc<MnList>, pub new: Arc<MnList>, pub diff: Arc<MnListDiff> }   // CH7a raises MnListChanged from it (§3.8)

/// The DML part of CDeterministicMNManager::UndoBlock (v23:736-769 / v24:784-817): the inverse change for listeners.
/// Nothing is erased; the active list simply becomes `prev_list`.
pub fn undo_block(cur: &Arc<MnList>, prev: &Arc<MnList>, stored: &MnListDiff) -> Option<MnListChange>;
```

- **`BlockPayloads`** is the block's special-transaction payloads, decoded once by CH7a and shared by EV1 and EV2b. Core decodes each payload twice (`GetTxPayload` in `CheckSpecialTxInner` and again in `RebuildListFromBlock`); decoding is a deterministic function of the bytes (and of `UngatedRules.protx_payload_decode`, EVO-14), so sharing is equivalent. A per-tx decode failure is kept and becomes `bad-protx-payload`/`bad-qc-payload` where Core would decode.
- **Reuse by the miner.** `node/miner.cpp` (v23:291 / v24:287) calls `BuildNewListFromBlock` for the CbTx root and `TestBlockValidity` calls it again. MN1's exact-block memo (A1 §2.13) keys `BuiltList` by the whole template and the prev tip.
- **`DmlTrace`** carries Core's `debugLogs` lines (§3.9); it defaults to no-op and never affects results.
- **Store faults are node errors, not block verdicts (manager ruling, LQ1 design review M1).** Core v24 catches `EvoDbInconsistencyError` separately in `ProcessSpecialTxsInBlock` and returns `state.Error(e.what())` so the block is **not** marked invalid (`specialtxman.cpp` v24:989-992; the type is `evodb.h` v24:45, thrown by `creditpool.cpp:156` and `mnhftx.cpp:313,377`); only `std::exception` becomes `failed-procspectxsinblock` (v24:993-996). A `QuorumMemberSource` that hits a read failure, or a missing record an established invariant requires (the MN list of a connected block), therefore returns `MemberError::Store`, which EV2b passes through as `DmlBuildError::Store`, and CH7a maps to the node-error path (abort, A1 I5). Normal absence is not a store fault: an absent snapshot row (`Ok(None)`) breaks the rotation and the remaining and new quarters are still computed, and unavailable work-block data (`BlockDataUnavailable`) selects the hash-form modifier, both as in Core (LQ1 §2.6, rev3); LQ1 tests the absence/store-error pair. The Opus leg mapped every member-source error to `failed-procspectxsinblock`; that would let a local DB fault mark a valid block invalid and is withdrawn. `MemberError::UnknownLlmqType` cannot reach EV2b (the type is checked first, §3.3); if it did it would map, as in LQ1's table, to `DmlException::Members` → `failed-procspectxsinblock`, which is Core's catch. **n5 correction:** v24's `state.Error` path is taken only for `EvoDbInconsistencyError`, which the credit-pool and MNHF code throw (`creditpool.cpp:156`, `mnhftx.cpp:313, 377`); the DML's own store faults in v24 (`ApplyDiff`'s `runtime_error`, `BlockDataUnavailableError` from `GetListForBlockInternal`) still fall into the `std::exception` catch. So "node error for every store fault" is the manager's ruling, matching v24's treatment of its *other* evo stores, not a statement that v24 already does it for the DML (§3.11 item 1).

### 2.12 Neighbours: who exchanges what

| Task | Direction | Types exchanged | Notes |
|---|---|---|---|
| EV0 | upstream | `EvoStore`, `EvoDelta`, `Effect`/`EffectSink`, **`QuorumMemberSource` + `MemberError` as in LQ1 §2.6 (rev3) (§2.11)**, the type skeletons | EV0 lands this document's §2 signatures as stubs (types first); if EV0 has not merged, EV2a lands them (PLAN §8.2). The member-source trait is defined once, by EV0, and LQ1 and EV2b both consume that definition |
| CS0 / CS5a | upstream | `Block`, `Tx`, `BlockId`, `ChainAncestry`, `DeploymentView`, `CoreWrite`/`HashWriter`, `BlockValidationError`, `UngatedRules`, `CoinsView` | EV2b reads no `UngatedRules` field (§3.10) |
| BS6a/c, BS11a-t | upstream | payload structs, `NetInfo`, `Service`, `DomainPort`, `PayoutShare`, `CollateralShare`; `OperatorKey` with `CBLSLazyWrapper` equality, the stored encoding and `reencode(legacy)` | §3.6 |
| BS5 | upstream | `DmlParams` from `ChainParams` | |
| EV1 | sibling (runs first) | `&MnList` at pprev: `get`, `has_unique`, `unique_owner`, `get_by_collateral`, `has_operator_key_under_any_scheme`; `BlockPayloads` | EV1 sees pprev; EV2b re-checks against the list as rebuilt so far |
| LQ1a/b | downstream (starts on EV2a-1, PLAN M-9c) and sibling | `resolve_list(work block)`, `iter_valid`, `counts`, `Dmn::{cold.confirmed_hash, cold.confirmed_hash_with_pro_tx, collateral, mn_type}` (through LQ1's `MnListView`); implements `QuorumMemberSource`, emitting `Effect::StoreQuorumSnapshot` into the sink EV2b passes | scoring sorts, so order is irrelevant; tie-break on collateral; store faults come back as `MemberError::Store` (node error) |
| LQ1b | downstream | `MnList::scratch`, `try_add` (exact `AddMN` failures, quirk 28), `get`, `is_pose_banned`, `counts().total()` | `utils.cpp` v23:339-420 / v24:343-466 |
| EV3 | downstream | `BuiltList.payee`; `Dmn::{operator_reward, cold.script_payout/payouts/shares/script_operator_payout}`; `payee()` for GBT/RPC | Core computes the payee twice (`payments.cpp` v23:55-60 / v24:152-157); we compute it once |
| EV4 | downstream | `iter()` (memcmp, SML root), `core_order()` (mnlistdiff), SML-hash slots, `MnListDiff`; `core_disk` readers; `MnList::new` + `try_add(bump=false)` | byte identity depends on `core_order` (quirk 41) |
| CH7a | downstream | `build_new_list_from_block`, `process_block`, `undo_block`, `DmlDelta`, `MnListChange`, `resolve_list`; it maps `DmlBuildError::Exception` | owns ordering inside `process_special_txs_in_block`, the views and the concurrent cache |
| GV1a/b | downstream | `counts().valid_weighted()`, `get_by_collateral`, `get_valid_by_collateral`, voting keys | |
| AM1, DK1, QM1, MP1a | downstream | `get_by_operator_key` (CHAMP-first), `get_by_service`, `get_valid`, `resolve_list` at quorum bases; `MnListChanged` | EVO-26 (MP1a) probes both encodings |
| RP2a/b, O14 | downstream | `core_order`, `diff_to`, `projected_payees`, `counts`, `MnStateDiff` field masks, `internal_id` | EVO-28/29 are RPC rows |
| SP1/AM1 | downstream | `Effect::ResetPlatformBan { pro_tx_hash, height }` | `CMasternodeMetaMan::ResetPlatformBan` → `SetPlatformBan(false, height)` (`masternode/meta.h:73-84`, `meta.cpp:187-194`, both pins) |
| dash-spv, desktop | none direct | | they consume EV4's `mnlistdiff`/`qrinfo` over P2P and RP2a's RPC (A1 §2.1 rule 8) |

---

## 3. Core equivalence

### 3.1 Functions replaced

| Core | v23.1.8 | v24.0.0-rc.3 | Rust |
|---|---|---|---|
| `CDeterministicMN` + serialize | `deterministicmns.h:43-94` | `:43-94` | `Dmn`, `codec` |
| `CDeterministicMNState` + serialize, `ResetOperatorFields`, `BanIfNotBanned`, `Revive`, `UpdateConfirmedHash` | `dmnstate.h:33-155` | `:33-175` | `MnHot` + `MnCold` |
| `CDeterministicMNStateDiff` | `dmnstate.h:156-286` | `:177-324` | `MnStateDiff` |
| `CDeterministicMNStateDiffLegacy` | `:288-446` | `:327-483` | `core_disk::decode_diff_d3_legacy` |
| `GetUniquePropertyHash`, `Add/Delete/UpdateUniqueProperty` | `deterministicmns.h:488-567` | `:526-614` | `UniqueKey`, `UniqueMap`, `list.rs` |
| `HasOperatorKeyUnderAnyScheme` | — | `:471-482` | `has_operator_key_under_any_scheme` |
| `IsEqual`, `operator==` | `:449-487` | `:487-525, 616-624` | `is_equal`, `PartialEq` |
| `GetMN…`, `GetMNByOperatorKey`, `GetMNByCollateral` | `deterministicmns.cpp:83-155` | `:57-130` | §2.6 |
| `CompareByLastPaid(_GetHeight)`, `GetMNPayee` | `:157-219` | `:132-194` | `payee.rs` (fast path); `scan::payee_scan` (literal, production fallback and twin) |
| `GetProjectedMNPayees` | `:221-274` | `:196-249` | `projected_payees` |
| `CalcMaxPoSePenalty`, `CalcPenalty`, `PoSePunish`, `DecreaseScores`, `PoSeDecrease` | `:292-359` | `:267-334` | `pose.rs`; twins `reference::{max_penalty_scan, decrease_scan}` |
| `BuildDiff`, `ApplyDiff` | `:361-418` | `:336-393` | `diff.rs`; twin `reference::build_diff_scan` |
| `AddMN`, `UpdateMN`, `RemoveMN` | `:420-647` | `:439-679` | `list.rs` |
| `CDeterministicMNManager::ProcessBlock`, `UndoBlock` | `:649-769` | `:689-817` | `process.rs` |
| `GetListForBlockInternal`, `CleanupCache` | `:778-947` | `:826-1014` | `store.rs` (+ CH7a's cache) |
| `RebuildListFromBlock`, `HandleQuorumCommitment` | `specialtxman.cpp:159-520` | `:283-795` | `build.rs` |
| `SetStateVersion`, `ApplyProUpServTx`, `IsSchemeMigration` | inline `:350-367`, `:396-404` | `:46-137` | `build.rs` (V24; pre-V24 subset proven equal, §3.2) |
| `SerializeCanonicalMNList`, `Unserialize…`, `CanonicalMNListHash`, canonical diff | — | `snapshot.h:237-394`, `snapshot.cpp:150-155` | `codec.rs` |

### 3.2 `build_new_list_from_block`: Core's order, step by step (quirk 25)

`h = prev.height + 1`. Every list operation runs on `new`, a clone of `prev_list` with its hash nulled and height `h` (`specialtxman.cpp` v24:317-323; v23:192-198). A `debug_assert` keeps Core's precondition `prevList == default || prevList.blockHash == hash(pprev)` (v24:317); in release it is `Invalid("dml-prev-list-mismatch")`, unreachable from CH7a and protecting the harness.

| # | Core (v23 / v24 lines in `specialtxman.cpp`) | Rust | Notes |
|---|---|---|---|
| 0 | `payee = prevList.GetMNPayee(pindexPrev)` (200 / 325) | `prev_list.payee(PayeeRules{v19, mn_rr})` | before any mutation (FR-EVO-02) |
| 1 | confirmations over **prevList**, banned included: null `confirmedHash` and `pindexPrev.height − registered ≥ minConf` → `UpdateConfirmedHash(proTx, hash(pindexPrev))` (205-218 / 330-343) | candidates from `unconfirmed.range(..=(prev.height − minConf, MAX))`, applied in `prev_list.core_order()` (§2.7 bulk rule) → new cold with both hashes, `update` | iterating `prev` and mutating `new` are equivalent because nothing above mutates `new` (Core's comment 327-329); the phase-start snapshot is `prev_list` itself. Mainnet 15: an MN registered at R is confirmed in `list(R+16)` with `confirmedHash = hash(R+15)` |
| 2 | `newList.DecreaseScores()` (220 / 345) | candidates from `decaying`, applied in the post-confirmation list's `core_order()`; `update_hot(penalty − 1)`; leaves `decaying` at 0 | only valid MNs (`deterministicmns.cpp` v24:309-334); state order-free, bucket order per §2.7 |
| 3 | for `i ≥ 1` (coinbase skipped by index), `IsSpecialTxVersion()` only, by `nType` (350-356) | one loop, block order | |
| 3a | ProRegTx (235-312 / 358-453) | §3.2.1 | |
| 3b | ProUpServTx (313-383 / 454-521) | §3.2.2 | EVO-15 |
| 3c | ProUpRegTx (384-413 / 522-586) | §3.2.3 | |
| 3d | ProUpRevTx (414-434 / 587-618) | §3.2.4 | |
| 3e | ProUpShareTx, ProUpSharedRegTx (— / 619-701) | §3.2.5 | V24 only |
| 3f | QcTx (435-461 / 702-728) → `HandleQuorumCommitment` (159-174 / 283-298) | §3.3 | interleaved in block order |
| 3g | any other type (ProDisTx included) | nothing | dissolution happens in step 4 (v24:744-747 comment) |
| 4 | second pass, `i ≥ 1`, **every** transaction, every input: `GetMNByCollateral(prevout)` with `collateralOutpoint == prevout` → V24 shared check → `RemoveMN` (464-482 / 731-757) | §3.5 | an MN registered and spent in one block is added then removed; a ProUpServTx for an MN spent later in the block is applied then removed |
| 5 | if the payee is still in `newList` (`GetMN`, banned allowed): `nLastPaidHeight = h`; Evo ∧ ¬MN_RR → `++nConsecutivePayments` (486-500 / 759-775) | `update_hot` | the bump happens even if the payee was banned or revoked in this block |
| 6 | over a copy `newList2`, all MNs: Evo ∧ `consecutive ≠ 0` ∧ ¬(it is the payee ∧ ¬MN_RR) → `consecutive = 0` (503-515 / 777-790) | candidates from `evo_streak`, applied in the post-payment list's `core_order()` (the `newList2` snapshot) | after MN_RR the payee is reset too |
| 7 | return `newList` with hash null; `ProcessSpecialTxsInBlock` sets it (v24:907-911; v23:620-624) | `BuiltList` | |

**3.2.1 ProRegTx** (v24:358-453; v23:235-312).
1. Payload, else `bad-protx-payload`.
2. `dmn.internal_id = new.total_registered` (v24:365); `pro_tx_hash = txid`; `collateral = (txid, n)` if the payload's hash is null, else the payload's outpoint (v24:366-373).
3. With `CollateralView::Coins` and an external collateral: the coin must exist, be unspent and be exactly `collat_amount(payload.nType)` (1000/4000 DASH), else `bad-protx-collateral` (v24:378-387). The view is the **pprev** UTXO set because `ProcessSpecialTxsInBlock` runs before `UpdateCoins` (`validation.cpp` v24:2661-2669; quirk 26). `Unchecked` skips it, as Core does when `view.GetBestBlock().IsNull()`.
4. Replacement: `if let Some(old) = new.get_by_collateral(&dmn.collateral) { new.remove(old) }` (v24:389-401): unique-map semantics, no equality check, exactly Core's. The replaced MN loses its payment position (fresh `registered_height`, `last_paid == 0`).
5. Each NetInfo entry: held in `new` → `bad-protx-dup-netinfo-entry`; neither AddrPort nor DomainPort → `bad-protx-netinfo-entry` (v24:403-415).
6. Shared (V24): any share key or the operator key held → `bad-protx-dup-key`; else owner **or** operator held → `bad-protx-dup-key` (v24:416-427). **platformNodeID is not re-checked here** (quirk 42, §3.6).
7. State `MnCold::from_pro_reg(payload, registered = h)` (`dmnstate.h` v24:74-89); empty NetInfo → `ban_if_not_banned(h)` (v24:431-437; since v23 an invalid `CService` decodes as empty, so the MN registers banned).
8. V24 only: `has_operator_key_under_any_scheme(key, null)` → `bad-protx-dup-key` (v24:443-446; EVO-13).
9. `try_add(dmn, bump_total = true)` (v24:448); any `Err` → `Exception` (a duplicate platformNodeID, or an owner key equal to an Evo's platformNodeID registered earlier in the block). The collateral slot is always free here: step 4 removed whatever MN owned it, whatever property type that slot held (a 36-byte service entry with the outpoint's bytes would remove the MN owning the *service*; cross-type removal, quirk 23/25; rev1, Opus n4).

**3.2.2 ProUpServTx** (v24:454-521; v23:313-383). Payload, else `bad-protx-payload`. Then, in the order **reachable** under each profile (§3.10): netinfo entries held by *another* MN in `new` → `bad-protx-dup-netinfo-entry` / `bad-protx-netinfo-entry` (v24:463-477; v23:319-333); MN missing → `bad-protx-hash` (v24:479-482; v23:335-338); `payload.nType ≠ dmn.mn_type` → `bad-protx-type-mismatch` (v24:483-485; v23:339-341); `!IsValidMnType` → `bad-protx-type` (v23:342-344 **after** the mismatch; v24:459-461 **before** the netinfo loop). Apply (`ApplyProUpServTx`, v24:110-137; v23 inline 346-367): pre-V24 sets `net_info`, `script_operator_payout`, and for Evo `platform_node_id` plus the two ports when `payload.version < 3`, else zeroed; V24 additionally pre-sets the version and runs `set_state_version(max(state, payload))` (§3.2.6). Revive if banned ∧ operator ≠ null ∧ voting ≠ null ∧ (shared ∨ owner ≠ null) (v24:494-504; v23:369-377 without the shared clause). V24: if `is_scheme_migration(old, target)` and the old key is held under either scheme by another MN → `bad-protx-dup-key` (v24:508-515). `update(...)`; `Err` → `Exception` (e.g. a platformNodeID taken earlier in the block).

**3.2.3 ProUpRegTx** (v24:522-586; v23:384-413). Payload; MN missing → `bad-protx-hash`; V24: shared MN → `bad-protx-shared-mn` (v24:532-535). `operator_changed = state.op != payload.op` by `CBLSLazyWrapper` equality, i.e. point equality for valid keys (v24:538; §3.6). Target version: pre-V24 `changed ? payload.version : old` (v23 writes `old > BasicBLS ? old : payload` when changed, equal because pre-V24 states are ≤ 2), V24 `max(old, payload)` (v24:539-540). V24: `(changed ∨ migrating) ∧ has_operator_key_under_any_scheme(payload.op, self)` → `bad-protx-dup-key` (v24:549-556). If changed: `reset_operator_fields()`, `ban_if_not_banned(h)`, `op = payload.op` (v24:558-563). `voting = payload.voting` (v24:564). `set_state_version(target)` (v24:568-570; pre-V24 this is exactly v23's `nVersion = …; netInfo = MakeNetInfo(nVersion)` at v23:401-402), failure → `bad-protx-netinfo-version`. Payout: `target ≥ 3` → `payouts` (payload's or `LegacyPayoutAsList`), `script_payout` cleared; else `script_payout = payload.script_payout`, `payouts` cleared (v24:571-579; v23:406 sets only `scriptPayout`). `update`.

**3.2.4 ProUpRevTx** (v24:587-618; v23:414-434). Payload; missing → `bad-protx-hash`. `target = v24 ? max(old, payload) : 1` (v24:602-605). `reset_operator_fields()`; if `target ≥ 2`, `set_state_version(target)` (V24 only; pre-V24 the version drops to 1, as in v23). `ban_if_not_banned(h)`; `revocation_reason = payload.reason` (v24:610-611). `update`.

**3.2.5 ProUpShareTx / ProUpSharedRegTx** (v24:619-701; V24 only): ported as written: `bad-proupshare-index`, `bad-proupshare-payee-reuse`; `bad-proupsharedreg-not-shared`, the operator-change reset, `SetStateVersion(old)`, `SetLegacy(false)` (v24:680), `bad-protx-dup-key`, `bad-proupsharedreg-payee-reuse`. Under `V23_1` these types never reach EV2b (EV1 rejects them, EVO-09); on mainnet never at all.

**3.2.6 `set_state_version`** (v24:51-106; FR-EVO-05): (1) target ≥ 3 and `payouts` empty while `script_payout` is not → migrate `script_payout` into `payouts` (v24:57-60); (2) re-encode a non-null operator key to `legacy = (target == 1)` (`Set`, not `SetLegacy`: the bytes change, so `UniqueKey::operator_key` changes and `update` re-keys the slot; v24:70-73); (3) return if the version and the NetInfo kind already match (v24:75-77); (4) else build a NetInfo of the target kind, copying CORE_P2P entries and, if the old one can store them, the Platform entries; an Evo whose legacy NetInfo is non-empty raised by anything but a ProUpServTx → `bad-protx-netinfo-version` (v24:79-96); (5) raising to ≥ 3 zeroes the two ports (v24:97-98). `reencode` is BS11a-t's; a stored key is always decodable (`IsTriviallyValid` required `IsValid()`), so it cannot fail on a validated chain; the L1 fuzzer can still produce an undecodable key, which maps to `Exception`, never a panic.

### 3.3 PoSe (quirk 24)

- `calc_max_pose_penalty() = max(100, counts.total())`: unweighted, banned included, **evaluated on the list as rebuilt so far** (`deterministicmns.cpp` v24:267-273; registrations earlier in the block count).
- `calc_penalty(p) = max × p / 100` (integer; v24:275-279).
- `pose_punish(m, x)` (v24:281-307): `penalty = min(max, penalty + x)` **even if already banned**; if not banned and `penalty ≥ max` → ban at `new.height`; `update_hot`. A missing MN throws in Core; unreachable after the `get` check.
- The QcTx branch (v24:702-728): payload, else `bad-qc-payload`; null commitments are ignored; unknown `llmq_type` → `bad-qc-commitment-type`; `quorumHeight = int(nHeight) − int(nHeight) % dkgInterval + quorumIndex` using the **payload's** `nHeight`, `i32` with C++'s truncating `%` (v24:712-714); `chain.ancestor(prev.id, quorumHeight)` missing or its hash ≠ `quorumHash` → `bad-qc-quorum-hash` (v24:715-719; `ancestor` returns `None` above `prev`'s height or below 0 as `GetAncestor` returns null); `members = members.members(type, base, effects)` (v24:723-725), where `Err(MemberError::Store)` → `DmlBuildError::Store` (node error, §2.11) and the snapshot effect LQ1b emits lands in the same sink Core writes at (`utils.cpp` v24:458-463); `handle_quorum_commitment` (v24:283-298): for each `i` **in member-vector order**, if `new.get(member)` exists and `!validMembers[i]` → `pose_punish(member, calc_penalty(66))`, recomputed per call. The punishments are applied in that order, never re-sorted by `core_order()` (§2.7; rev3, GPT R3-m1): within a collision bucket the punished members end up in the reverse of their member-vector subsequence (`quirk_41_qctx_order`).
- **Reachable domain.** In a block, LQ2's `CheckLLMQCommitment` has enforced `nHeight == h` and `validMembers.len() == size ≥ members.len()`. Outside it Core's arithmetic can overflow `int` (UB) and `validMembers[i]` can read out of bounds. EV2b uses `wrapping_*` and returns `Exception(OutOfDomain)` for an out-of-range index (§3.11). The L1 generator stays inside the reachable domain.

### 3.4 Payee selection and projections (FR-EVO-04; quirk 13)

```rust
fn pay_height(h: &MnHot, c: &MnCold) -> i32 {                // CompareByLastPaid_GetHeight (v23:157-166 / v24:132-141)
    let mut height = h.last_paid_height;
    if h.pose_revived_height != -1 && h.pose_revived_height > height { height = h.pose_revived_height; }
    else if height == 0 { height = c.registered_height; }
    height
}
```
Note the `else if`: a revived height that is set but not above `last_paid_height` leaves `last_paid_height` in force, and the `registered_height` fallback applies only when `last_paid_height == 0` *and* the revived branch did not fire.

- **The queue** holds `PayeeKey { pay_height, pro_tx }` for every **valid** MN, unconfirmed ones included (`ForEachMNShared(onlyValid=true)`). `CompareByLastPaid` is a strict total order, so Core's linear minimum equals `payee_queue.first()` whatever the iteration order.
- **Two paths, one contract (rev2, ruling R2-F2).** `payee()` and `projected_payees()` must equal Core's literal algorithm on **every** list the canonical reader accepts, and the reader is not made stricter than Core's. The indexed path is used only when two preconditions hold, which are checked, never assumed:
  - **P1 (I-P):** at most one valid MN has `last_paid_height == list.height`;
  - **P2:** `list.height > 0`, or every MN has `registered_height ≥ 0` (so `pay_height ≥ last_paid_height` for every MN: the `else if` substitutes `registered_height` only when `last_paid == 0`, and a negative registration can then put a `last_paid == 0 == height` candidate *below* the `pay_height ≥ list.height` range the fast path walks; Core's reader checks the list height, not registration heights).

  `fast_payee_ok: OnceLock<bool>` holds the conjunction and is **always computed from the list itself, lazily, the first time `payee`/`projected_payees` is called**; every mutation clears it, and `build_new_list_from_block` certifies nothing (rev3, GPT R3-F2, ruling 2). The rev2 claim that one build repairs P1 assumed `last_paid ≤ prev.height` for every MN, which Core's reader does not check: with a predecessor holding two Evo MNs at `lastPaid = 101` and a regular C at 1 (height 100), the actual v24 `RebuildListFromBlock` for block 101 pays C, keeps A's and B's values and resets their streaks, so the **result** has three valid MNs at `lastPaid == 101` and Core's projection is `A A A A A A A A B` (`/work/scratch/ev2-gpt3/rebuild_probe.cpp`); a cache set `true` by the builder would have hidden that. Relying on no predecessor provenance is the safe rule, and it is cheap, because the check is the candidate search itself:
  - **P2** is `list.height > 0`, or else (height 0, never a built list) one O(n) pass over `registered_height`.
  - **P1** is "at most one valid MN with `last_paid_height == list.height`". The precise implication (rev4, R4-m2) is **`list.height > 0 ∧ last_paid == list.height ⇒ pay_height ≥ list.height`**: for such an MN `last_paid ≠ 0`, so the `registered` substitution cannot fire, and a revival can only raise the key. It is *not* true that `height > 0` bounds every MN's pay height from below by its `lastPaid`: a valid MN with `lastPaid = 0, registered = −2, revived = −1` has pay height −2 and the reader accepts it; it simply is not an MN being counted. So counting the P1 set is one walk of `payee_queue.range(PayeeKey { height: list.height, pro_tx: MIN }..)` filtered by `last_paid == height`: O(r log n) with r the number of valid MNs whose pay height is at least the list height, a handful on any built list and at most n on a reader-accepted one. At height 0 the count is taken in the same O(n) pass as P2. The twin recomputes both by a literal `ForEachMNShared` and xcheck compares the two counts; it never asserts the count is ≤ 1 (§2.4 provenance rule).
  This is the "cheaper invariant-based route" ruling 2 allows: it is provably equivalent to tracking the count during the build, because it counts the same set on the same resulting list, and it also covers lists no build produced. When `fast_payee_ok` is `false`, both functions run **`scan::payee_scan` / `scan::projected_scan`**: Core's code over `core_order()`, in `scan.rs`, compiled in every build (only the twin is behind `xcheck`; rev3, R3-m2). Core's reader accepts all the reviewer fixtures (two valid Evo MNs at `lastPaid == 100 == height`; a height-0 list with registrations at −2 and −1; the read → build fixture above), and on them the fallback is what runs.
- **`payee(r)`** (v24:158-194): an empty list → `None`; every MN banned → `None` (Core `nullptr`; EV3 then accepts any coinbase, quirk 13). Fast path (P1 ∧ P2): if `r.v19 ∧ ¬r.mn_rr` (v24:166-181), walk `payee_queue.range(PayeeKey { height: list.height, pro_tx: MIN }..)` (**the unbounded range `pay_height ≥ list.height`**, not an equality run: rev3, GPT R3-F1) and keep the entry satisfying Core's exact predicates, `last_paid_height == list.height ∧ valid ∧ Evo ∧ consecutive < 4` (`dmn_types::Evo.voting_weight`); P1 makes that entry unique, P2 guarantees it is in the range (a continuation candidate's `pay_height` may *exceed* the list height when its `nPoSeRevivedHeight` is in the future, which the reader does not forbid: the reviewer's fixture `E: lastPaid 100, revived 101` has queue key `(101, E)` and Core pays E, projection `E E E R E`); if one is found it is returned, otherwise `payee_queue.first()`. Literal path: `ForEachMNShared(onlyValid)` over `core_order()`, last match wins (v24:171-179), then the linear minimum. The pre-V19 gate is kept for line parity although no Evo exists before V19 (EV1 rejects Evo registrations before V19).
- **Across versions.** `CompareByLastPaid`/`GetMNPayee` are byte-identical at both pins. From MN_RR (mainnet 2,128,896): no continuation, no streak bump, step 6 resets the payee's streak.
- **`projected_payees(r, n)`** (v24:196-249). Literal algorithm: pre-MN_RR, the first scan over valid MNs in CHAMP order **appends the remaining payments of every MN** with `last_paid == height ∧ Evo ∧ consecutive < 4` as it meets them (v24:213-225), while `remaining_evo_payments` and `evo_to_be_skipped` keep only the **last** match; the weighted scan then skips that last MN only; its already-paid copies go to the end; `std::sort(begin + remaining, end, CompareByLastPaid)`; `resize(min(n, weighted_count))`. With several candidates (P1 false) the earlier candidates' entries survive unsorted at the front, which is why the fast path is gated on P1: the GPT reviewer's fixture gives Core `A A A A A A A B` where a last-candidate model gives `B B A A A A B B`. Fast path (P1 ∧ P2): the candidate found by the same range walk as `payee()` (if any) contributes its remaining copies first, then the queue order with each valid MN repeated `voting_weight` times (once after MN_RR) and the candidate's paid copies at its sorted position; equal keys are the same MN, so `std::sort`'s instability cannot show. Literal path otherwise (`scan::projected_scan`).
- **The reference twin** (`reference.rs`) is Core's code over Core's data layout: three `imbl` maps plus its own `collision_buckets`, every bulk phase applied literally (iterate a CHAMP-ordered snapshot, `set` each, as §2.7 describes), linear scans for payee and projection (`payee_scan`, `projected_scan`, which double as the fallback above), unique keys recomputed per `Add`/`Update`/`Remove`, `GetMNByCollateral` plus the equality check per input. **Sharing contract (rev4, R4-n1):** the twin shares with the fast path the payload decoder, the canonical encoder, and `scan.rs`'s literal `payee_scan`/`projected_scan`; nothing else. Because the fallback inputs (P1- or P2-violating lists) therefore send production and twin through the same scan, the twin is not an independent check of the scan itself; the independent check of `scan.rs` is the **direct Core differential coverage** of fallback inputs (`diff_mnlist_order` on reader-accepted lists against `oracle_mnlist_payee`/`oracle_mnlist_projected`, §6.2–6.3), which the GPT round-4 randomized comparison (25,000 lists per pin, every deployment context) already exercised against the design model. The twin remains the `-consensusxcheck` partner and the benchmark baseline (A1 §3.10).

### 3.5 Collateral spends, including ordinary transactions (quirk 25)

For every input of `vtx[1..]`: `new.get_by_collateral(prevout)` and `dmn.collateral == prevout` (v24:736-737). With the pre-image key this is Core's lookup to the byte: the slot owner of the 36 outpoint bytes, and the equality check guards the same cross-type collision (a 36-byte TORv3/I2P service entry with identical bytes) in both implementations. V24: a shared MN's collateral spent by anything other than a ProDisTx → `bad-shared-collateral-spend` (v24:738-743); the test is on the **raw** `tx.nType != TRANSACTION_PROVIDER_DISSOLVE` with no `IsSpecialTxVersion()` guard (v24:739), which EV2b copies for fuzz parity with `oracle_dml_apply` (a valid block cannot reach the difference: `ContextualCheckTransaction` forbids a non-zero `nType` below version 3, `validation.cpp` v24:375-377; rev1, Opus n2). `remove`; an `Err` is impossible by I-U but maps to `Exception`. Cost: one hash-map probe per input, no SHA256d (Core pays one per input).

### 3.6 Unique properties and BLS key semantics (quirk 23; quirk 42)

- **`OperatorKey` contract** (to BS11a-t). EV2 needs `CBLSLazyWrapper` behaviour (`bls.h` v23:393-577 / v24:392-577; `operator==` at :514 in both): equality is bytes when both are raw with the same scheme, points when both are decoded, else decode both, invalid decodes comparing equal to each other; the stored bytes and scheme (for `UniqueKey` and serialization); `reencode(legacy)`; `is_null`. Keys in states are always valid and canonical (EV1's `IsTriviallyValid` requires `Get().IsValid()`), so the invalid-equals-invalid corner is unreachable from a list but kept.
- **Cross-type collisions are reproduced:** a ProRegTx whose `keyIDOwner` equals an existing Evo's platformNodeID gets `bad-protx-dup-key` (step 6 probes the owner key, the slot exists); in the opposite order within one block (owner first, then an Evo ProRegTx with that platformNodeID) step 6 misses it and `try_add` fails → `failed-procspectxsinblock`. In a later block EV1 rejects it with `bad-protx-dup-platformnodeid`.
- **Quirk 42 (proposed): in-block list exceptions and order-dependent strings.** Core's explicit in-block re-checks cover only some properties: ProRegTx re-checks NetInfo, owner and operator but not platformNodeID; ProUpServTx re-checks NetInfo only; ProUpRegTx re-checks nothing pre-V24. The remaining conflicts surface as `AddMN`/`UpdateMN` throws → `failed-procspectxsinblock` (`BLOCK_CONSENSUS`), and which string a block gets depends on transaction order. Named tests with Core's verdict from the oracle: two ProRegTxs in one block with the same platformNodeID; owner keyID vs platformNodeID in both orders; two ProUpRegTxs moving to the same operator key (pre-V24; V24 gets `bad-protx-dup-key` from the probe); two ProUpServTxs claiming the same platformNodeID.

### 3.7 State diffs (`MnStateDiff::between`)

v24 compares `pubKeyOperator` by `GetHash()` of its encoding (`dmnstate.h` v24:259-268), v23 by point (v23:221-242). They differ only when a key keeps its point but changes encoding, which only `set_state_version` under V24 rules causes. Pre-V24 the encoding is a function of the version, and the version changes only together with the key (ProUpReg with a changed key; ProUpRev to null), so on mainnet and under `V23_1` both rules give identical diffs. ~~We implement v24's byte comparison~~ (withdrawn, see the erratum below). The `VERSION`-forcing rule (v24:276-280) and the serialization guard (v24:292-295) are identical at both pins.

**Erratum (BL-67, 2026-10-10; PLAN §7, EMBARGO-5).** The conclusion above, that the byte rule and the point rule give identical diffs on mainnet, is withdrawn. The reason is private (EMBARGO-5), and EV2a's implementer receives it under the PLAN §7 grant. The rules are now:
- `between` records `PUB_KEY_OPERATOR` whenever the key's **point** (the Core-style lazy-wrapper identity of §3.6) **or** its encoding changes. This is the union of both pins' triggers, so a list rebuilt from diffs and snapshots is bit-identical to the online list. Importing Core's own diffs is unchanged: the decoder reads whatever fields Core wrote.
- Every "operator changed" decision (ProUpReg, ResetOperatorFields, BanIfNotBanned) uses the point identity of §3.6, never the bytes or their hash.
- No any-scheme duplicate rule applies outside V24-gated paths. Before porting each call site, confirm its gating.
- **Required test:** restart or rebuild between any operator-key change and the next block, then require the rebuilt list (and SML root) to equal the online list.
- EV0's `dash-dml/src/diff.rs` pointer to "§2.8, §3.7" for `between` now means this erratum.

### 3.8 `process_block`, `undo_block`, effects, and who owns what

Core's `CDeterministicMNManager::ProcessBlock` (`deterministicmns.cpp` v23:649-734 / v24:689-782) runs only when DIP3 is active at the block (v24:696-698) and `fJustCheck` is false (`specialtxman.cpp` v23:628 / v24:913). Its order, which `process_block` reproduces:

| # | v23.1.8 | v24.0.0-rc.3 | Rust |
|---|---|---|---|
| 1 | `newList.to_sml()` (populate the SML cache) | same | EV4's concern; not here |
| 2 | `oldList = GetListForBlockInternal(pprev)`; `diff = oldList.BuildDiff(newList)` (:669-671) | same (:709-711) | `diff = prev_list.diff_to(&new_list)` |
| 3 | **`ResetPlatformBan(proTxHash, nHeight)` for every ProUpServTx** `i ≥ 1` whose payload decodes (:673-688) | `WriteDerived(DB_LIST_DIFF)`, then the snapshot write, **then the same loop** (:713-750) | `effects.emit(Effect::ResetPlatformBan { pro_tx_hash, height: h })` in block order |
| 4 | `Write(DB_LIST_DIFF)`; snapshot if `h % 576 == 0 ∨ pprev == m_initial_snapshot_index` (:690-696) | (done at 3) (:716-731) | `DmlDelta { diff, snapshot }` |
| 5 | caches; catch → `failed-dmn-block` (:698-705) | same (:751-757) | nothing can fail here; the string is documented as unreachable |
| 6 | `updatesRet` only if `diff.HasChanges()` (:707-709) | always (:759) | `MnListChange` always; CH7a raises `MnListChanged` only when the diff has changes, at both pins (superseded "per profile": EV0 erratum after §8 Q12; CH7 D15) |
| 7 | `bad-dip3-enf-block` if `h == DIP0003EnforcementHeight` and the pinned hash differs (:722-729) | same (:769-776) | `Err(Invalid("bad-dip3-enf-block"))` |

- **The effect carries the height** (`ResetPlatformBan(const uint256&, int height)` → `SetPlatformBan(false, height)`, `masternode/meta.cpp:187-194`, `meta.h:73-84`, both pins), and `SetPlatformBan` refuses a stale or equal height. **A1 §2.10.1 erratum:** `Effect::ResetPlatformBan { pro_tx_hash }` → `{ pro_tx_hash, height }`.
- **It is emitted before the enforcement check at both pins**, so it survives `bad-dip3-enf-block`. It never runs in just-check mode, GBT proposals or `TestBlockValidity`, because Core does not call `ProcessBlock` then; CH7a reproduces that gate by not calling `process_block`. This satisfies A1 §2.10.1's rule ("pure functions that can produce effects take `&mut dyn EffectSink`") and A1 §2.5 ("platform-ban resets go to the `EffectSink` parameter of `process_special_txs_in_block`"): the parameter is threaded from CH7a to `process_block`.
- **Member effects.** When LQ1b's source computes rotated members inside the QcTx branch it stores the snapshot, as `GetAllQuorumMembers` → `BuildNewQuorumQuarterMembers(storeSnapshot = true)` does on a cache miss (`utils.cpp` v24:458-463). `build_new_list_from_block` passes the same `EffectSink`, so the emission happens in Core's place. Whether Core hits its static cache is history-dependent: an "observation"-class digest field (A1 §4.6).
- **`MnListChanged` is not an effect.** (Superseded by the EV0 erratum after §8 Q12: both pins notify only when the diff has changes, and CH7 D15 follows it.) CH7a raises it after a successful connect: in v24 always (`validation.cpp` v24:2918-2920), in v23 only when the diff has changes (v23:2709-2713 uses the `optional` that `ProcessBlock` fills only then). Listeners are process-internal (DKG, MN connections); CH7a decides whether the difference needs an `UngatedRules` row (handed to CH7a/O17).
- **`WriteDerived` mismatches** (v24:716-726) are local-corruption aborts, never block verdicts; our store treats a differing existing row the same way (`StoreError::Inconsistent`, fail-stop).

**Ownership table.**

| Core step | Owner | Where |
|---|---|---|
| `RebuildListFromBlock`, every reject string in it, the throw → `failed-procspectxsinblock` mapping *point* | EV2b builds; **CH7a maps** `DmlBuildError::Exception` at Core's catch (`specialtxman.cpp` v23:701-704 / v24:993-996) and `DmlBuildError::Store` at Core's `EvoDbInconsistencyError` catch (v24:989-992: `state.Error`, node error, block not marked invalid) | `build.rs`; CH7a |
| `QuorumMemberSource` trait and `MemberError` (text: LQ1 §2.6 (rev3)) | **EV0** defines; LQ1a/b implement; EV2b and LQ2 consume | `dash-evo` |
| `BuildDiff`, snapshot cadence, `DmlDelta` | **EV2b** | `process.rs` |
| `Effect::ResetPlatformBan { pro_tx_hash, height }` emission, in Core's position | **EV2b** (`process_block`, via the `EffectSink` CH7a passes) | `process.rs` |
| the `!fJustCheck` and DIP3-active gates around `ProcessBlock` | **CH7a** | `process_special_txs_in_block` |
| `bad-dip3-enf-block` | **EV2b** (`process_block`; the hash and height are `DmlParams` from BS5, quirk 34's rule site) | `process.rs` |
| applying `ResetPlatformBan` (`SetPlatformBan(false, h)`, "ignore an MN without meta", "refuse a stale height") | **SP1/AM1** | `dashd-masternode` meta store |
| `MnListChanged` event | **CH7a** | from `MnListChange` |
| persisting `DmlDelta` (B1 Durable batch), the concurrent `ListCache`, `EvoReadView` | **CH7a / ST1** | `dashd-chainstate` |
| `EvoStore` trait, `Effect`/`EffectSink`, `QuorumMemberSource` definitions | **EV0** (EV2a lands them if EV0 has not merged) | `dash-evo` |

### 3.9 Reject strings and classes

Every explicit reject is `state.Invalid(BLOCK_CONSENSUS, reason)` with no debug message; returned as `DmlBuildError::Invalid(BlockValidationError::new(Consensus, reason))`.

| Reason | Where | Pins |
|---|---|---|
| `bad-protx-payload` | 3a–3e payload decode | both |
| `bad-protx-collateral` | 3a, `Coins` only | both |
| `bad-protx-dup-netinfo-entry`, `bad-protx-netinfo-entry` | 3a, 3b | both |
| `bad-protx-dup-key` | 3a; V24 probes in 3a/3b/3c/3e | both (probes v24 only) |
| `bad-protx-type` | 3b (first in v24, last in v23; EVO-15) | both |
| `bad-protx-hash` | 3b–3e | both |
| `bad-protx-type-mismatch` | 3b | both |
| `bad-protx-netinfo-version` | `set_state_version` | v24 (V24) |
| `bad-protx-shared-mn`, `bad-proupshare-index`, `bad-proupshare-payee-reuse`, `bad-proupsharedreg-not-shared`, `bad-proupsharedreg-payee-reuse` | 3c, 3e | v24 (V24) |
| `bad-qc-payload`, `bad-qc-commitment-type`, `bad-qc-quorum-hash` | 3f | both |
| `bad-shared-collateral-spend` | 4 | v24 (V24) |
| `failed-procspectxsinblock` | any `MnListError` (or `OutOfDomain`) inside the build; mapped by CH7a at Core's `std::exception` catch | both |
| *(node error, no reject string)* | `DmlBuildError::Store` / `DmlStoreError::Inconsistent`: a store fault in the member source or the list store; CH7a maps it to `state.Error` + abort, the path v24 takes for `EvoDbInconsistencyError` (v24:989-992). Neither pin takes it for DML faults (§3.11 item 1): a ruling, both profiles | ruling |
| `failed-dmn-block` | `process_block` internal error (Core's `try`, v23:665-705 / v24:705-757); unreachable in Rust | both |
| `bad-dip3-enf-block` | `process_block` | both |

A census test asserts the set equals the strings in both pins' `RebuildListFromBlock` and `ProcessBlock`. **Logging:** `ProcessSpecialTxsInBlock` builds with `debugLogs = true`, so Core logs every registration, update, revocation, removal, PoSe punishment and ban with fixed texts (`specialtxman.cpp` v24:396-399, 450-452, 501, 518-520, 745-749; `deterministicmns.cpp` v24:300-304). EV2b emits them through `DmlTrace` as `tracing` events with Core's text and category (A1 Appendix E) for ND1 (NFR-OPS-02) and O11's asserted-log extraction.

### 3.10 Per-profile and per-hardfork behaviour (A1 §2.25, O17)

| Behaviour | Keyed by | v23.1 profile | v24.0 profile | Evidence |
|---|---|---|---|---|
| ProUpServTx `nType` check position (EVO-15, `profile`, `UngatedRules.proupserv_type_in_special_tx`, O17 §4.1) | **nothing in EV2b.** Under `V24_0` EV1 rejects an out-of-range or mismatched `nType` in `CheckProUpServTx` (`providertx.cpp` v24:420-422; `specialtxman.cpp` v24:1344-1350) before `RebuildListFromBlock` runs (the check loop at v24:862-875 precedes the build at v24:907), so inside EV2b the position of `bad-protx-type` is dead code. EV2b implements v23's reachable order once: netinfo → hash → mismatch → type | that order | same code; unreachable | v23 `specialtxman.cpp:319-344`; v24 `:459-485`. Tests (rev1, Opus m2 / GPT F4; each names the rejecting layer): (a) a ProUpServTx with unknown `proTxHash` and `nType = 7` in a **validated block**: under `V24_0` **EV1** rejects with `bad-protx-type` (`GetValidatedPayload` → `IsTriviallyValid`, `providertx.cpp` v24:420-422, runs before the hash lookup at `specialtxman.cpp` v24:1339-1341); under `V23_1` **EV1** rejects with `bad-protx-hash` (v23 `specialtxman.cpp:1082-1085`; v23's `IsTriviallyValid` has no type check). Neither verdict comes from EV2b. (b) The same tx with a known hash: `V23_1` EV1 passes it and **EV2b** gives `bad-protx-type-mismatch`; `V24_0` EV1 gives `bad-protx-type`. (c) To reach EV2b's own order under both profiles: a block where a ProRegTx replaces MN X by collateral reuse and a later ProUpServTx targets X → **EV2b** `bad-protx-hash` under both. (d) Raw-helper difference, tested as an *expected* difference (§6.3): the bare `RebuildListFromBlock` on (a) gives `bad-protx-hash` in v23 and `bad-protx-type` in v24; ours gives v23's. A unit test asserts `build_new_list_from_block` reads no `UngatedRules` field, so O17's consumer list (EV1, MP1a, P4, MN1) stays correct and Opus's proposed erratum is withdrawn |
| ProTx version handling, `set_state_version`, cross-scheme probes, shared MNs, payout lists, ExtAddr | `DmlRules.v24` (EVO-08, -10, -11, -13; `gated`) | v24's rules if V24 were active (Q1) | v24's rules | v24 `specialtxman.cpp:51-137, 416-446, 488-515, 532-616, 619-701, 739-743` |
| BLS encoding of the stored key | `MnCold.version`, never the global flag | same | same | `dmnstate.h` v23:97 / v24:107; CONS-09's leak does not reach the DML |
| `UpdateUniqueProperty` for BLS (point vs hash) | — (pre-V24 unreachable) | same results | same results | §2.5 |
| Diff serialization order; snapshot at the first DIP3 block; cache bounds (EVO-25 `none`) | — (storage) | ours | ours | §2.8, §2.10 |
| `MnListChanged` only-on-change (v23) vs always (v24); superseded: both pins notify only on changes (EV0 erratum; CH7 D15) | CH7a's | | | §3.8 |

| Hardfork | DML effect |
|---|---|
| DIP3 (`active_at(block)`; mainnet 1,028,160, testnet 7,000, devnet 2, regtest 432 by default) | the DML exists; below it the list is the default list (height −1) |
| DIP3 enforcement (mainnet 1,047,200, testnet 7,300) | `bad-dip3-enf-block` hash pin in `process_block` |
| V19 (`active_after(prev)`) | Evo MNs can exist (EV1); the evonode continuation in `payee` when ¬MN_RR; the BLS scheme flip is CH7a's |
| V20 | none in the DML |
| MN_RR (`active_after(prev)`) | no continuation, no streak bump; step 6 resets the payee's streak; `projected_payees` stops weighting |
| V24 (`GetSpecialTxRules(pprev).v24`) | §3.2.2–3.2.6 V24 branches; the shared-collateral spend rule |
| `evo_shares` | none in the DML (EV1 gates registrations and payouts) |

### 3.11 Declared deviations (all internal or unreachable)

1. **Local store faults are node errors** (A1 I5; manager ruling): a missing list or block in the list store, or a read failure or invariant-required missing record in the member source, aborts the node and never produces a block verdict; the member source's normal absences (an absent snapshot row, unavailable work-block data) are values, not faults (LQ1 §2.6, rev3). v24 does this for `EvoDbInconsistencyError` (credit pool, MNHF; `specialtxman.cpp` v24:989-992) but **not** for the DML's own faults (`ApplyDiff` `runtime_error`, `BlockDataUnavailableError`), which both pins map to `failed-procspectxsinblock` (rev1, Opus n5). Both profiles get the node-error behaviour (unobservable without a hardware fault).
2. **Out-of-domain QcTx arithmetic** (§3.3) gets defined behaviour where Core has UB; unreachable after EV1/LQ2.
3. **Collision buckets and host width (rev1).** The order inside a 64-bit `ReadLE64` collision class is reproduced for the from-genesis online 64-bit history (§2.7). Three Core inconsistencies are recorded, not emulated: a Core node that reconstructed the list from `dmn_S3`/`dmn_D4` orders its buckets differently (reload reverses, `ApplyDiff` re-prepends), an imported datadir differently again, and a 32-bit host builds a 7-level trie. Reaching a collision needs a ~2³² birthday search over the attacker's own ProRegTx txids; v24's canonical reader caps runs at 8.
4. **Registration counter after reconstruction (rev1).** Our reconstructed lists carry the online `total_registered` (§2.8); Core's reconstructed lists do not, so a Core node that restarted across a register-and-spend block has a lower counter, reused internal ids and a different canonical hash from its own online history. Our target is the online history; the O8 digest run must come from a node that did not restart inside the window, and the harness reports a mismatch confined to `totalRegistered`/`internalId` as oracle provenance, not as a DML divergence (§6.4).
5. **Snapshot cadence (first DIP3 block), diff `updated` order, cache policy** are storage choices (DEC-11, §2.10).

### 3.12 Quirks owned or relied on, and the two proposed

| Quirk | Statement | EV2 test |
|---|---|---|
| 13 | payee tie-break by memcmp; unconfirmed payable; all-banned → `None` | `quirk_13_*`: queue vs twin on tie-heavy lists (equal `pay_height`, hashes differing in the first or last internal byte); unconfirmed payee; all banned |
| 17 (slot only) | SML leaf hash excludes `nVersion` | the cold-state slots survive hot updates and `clone`; EV4 fills them |
| 23 | one map keyed by `SerializeHash`; keyID/platformNodeID collision; per-encoding operator slots pre-V24 | `quirk_23_*`: both collision orders; one point under two encodings held by two MNs pre-V24 (allowed) and rejected under V24 (EVO-13); ADDRv1 vs ADDRv2 hashing; CJDNS 20-byte and TORv3 36-byte length classes |
| 24 | PoSe added to banned MNs; `max(100, total)` on the partly rebuilt list | `quirk_24_*` |
| 25 | update order; ProRegTx with the same collateral replaces | `quirk_25_*`: register-and-spend; replacement keeps neither payment height nor internalId; QcTx between two ProTxs; ProUpServTx for an MN spent later in the block |
| 26 (DML half) | external collateral from the pprev UTXO set | a ProRegTx whose collateral was created earlier in the block → `bad-protx-collateral` (`Coins` only; EV1 rejects first in a real block) |
| 28 (contract) | LQ1b swallows `AddMN` throws on scratch lists | `try_add` failure-order tests |
| **41** (registered, DEC-50(4); owner EV2a; consumers EV4, RP2a, O14, AM1, MP1a) | Core's iteration order is immer CHAMP order over `ReadLE64(proTxHash)` on 64-bit hosts, with collision nodes in most-recently-set-first order (§2.7); 32-bit hosts and reconstructed or imported Core lists are Core inconsistencies | `quirk_41_*`: vs a naive trie; vs the oracle (`protx list`, `oracle_mnlist_core_order(ops)`); 60-bit shared prefixes; crafted 64-bit collisions through insert, hot update, cold update, replacement, removal and re-registration; **bulk decay, confirmation and streak phases over a colliding bucket (`quirk_41_bulk_decay`, §6.2)**; `get_by_operator_key` first-match on a colliding pre-V24 cross-encoding pair, after a full block build and after reconstruction |
| **42** (registered, DEC-50(4); owner EV2b) | in-block list exceptions → `failed-procspectxsinblock`; order-dependent strings (§3.6) | the four cases, against both pins |

Numbering: R4 §1.15 has 1–34, rev1 added 35–38 (PLAN O5), CS1 registered 39 (DEC-25), CS2 registered 40 (DEC-29); 41 and 42 are the next free numbers. Fable's design listed quirk 26 among its inputs; it is R4's existing quirk 26, not a new proposal, so the single list of new quirks is **41, 42**.

---

## 4. Persistence, crash consistency and recovery

EV2a stores nothing itself; it defines what is stored and how it is read back.

- **Column families** (A1 §2.9): `mn_diff` (block hash → envelope ‖ canonical diff) for **every** DIP3-active block, empty diffs included (Core writes an empty `CDeterministicMNListDiff`, and the walk needs the row to tell "no changes" from "initial snapshot"); `mn_snapshot` (block hash → envelope ‖ canonical list) when `DmlDelta::snapshot` is `Some`. Both travel in the **B1 Durable batch** of A1 §3.5.4 with the block index, the other evo deltas and `meta.evo_tip`.
- **Never erased.** Disconnects keep diffs and snapshots (Core's `UndoBlock` only reads; A1 §2.9 "kept"), so reorgs reconnect previously seen blocks without recomputing storage. A rewrite of an existing key is a pure function of `(list(pprev), block, rules)` and byte-identical; xcheck builds compare the bytes as v24's `WriteDerived` does (`evodb.h` v24:150-205) and abort on a mismatch.
- **Recovery** (A1 §3.5.5): evo state is atomic at `meta.evo_tip`; coin replay never touches it. On startup the tip list is `resolve_list(evo_tip)`: at most 575 diffs over the nearest snapshot, or the optional tip snapshot (Q8). Blocks above the recovered tip reconnect through ordinary validation and rewrite identical rows. CH7a's `recover` also asserts that `resolve_list(evo_tip)` needs no initial-snapshot fallback above `DIP3Height`; otherwise "Found EvoDB inconsistency, you must reindex" (Core's text).
- **Effects** (`ResetPlatformBan`) persist through the meta store's own flush, as Core's `mncache.dat` does: "no more durable than Core" (A1 §2.10.1).
- **Decoder robustness.** Every codec is total over arbitrary bytes with bounded allocation (the canonical limits of §2.9; the `core_disk` readers bound compact sizes by the remaining input), because EV4's importer and `EvoSnapshot` files are untrusted input; the corpus fuzzers (§6.2) run them. `resolve_list` runs `check_invariants()` on every decoded snapshot in debug/xcheck builds; release builds rely on the canonical checks.
- **Schema evolution:** a new `codec_id`, never a mutation of an existing one.
- **Tests:** ST3a's `FaultyEngine` model with CH7a: crash at every sub-batch boundary across a cycle-base height, a 576-multiple, a register-and-spend block and a reorg; the reloaded tip list **and every reconstructed historical list in the window** have the same canonical bytes, `total_registered` and `core_order()` as an uncrashed run (rev1, GPT). ST3b device-level runs come with CH7a.

---

## 5. Performance

### 5.1 Metrics and budgets (never slower)

| PRD / B1 metric | How the DML enters | Budget |
|---|---|---|
| 1–3 IBD / reindex (post-DIP3 windows), 12 CPU per block | `build_new_list_from_block` + `process_block` per block; the diff encoding into B1 | PF0 `reindex-chainstate` counters over the post-DIP3, post-V20 and tip−50k windows ≤ the previous head; DML instructions per block ≤ dashd's (Core bench patch, §5.5) |
| 5 per-block connect latency | the same work on the connect critical path | ≤ dashd's `BuildNewListFromBlock` + "m_dmnman.ProcessBlock" (`-debug=bench`); target 0.5× |
| 8 GBT | the payee once; the build reused through MN1's memo | ≤ dashd |
| 9 startup | the tip list from a snapshot + ≤ 575 diffs | ≤ dashd's first `GetListAtChainTip` |
| 10 memory | the list cache | ≤ dashd's `mnListsCache` + `mnListDiffsCache` |
| 11 disk | `mn_diff` + `mn_snapshot` vs evodb `dmn_D4` + `dmn_S3` | ≤ dashd per component (same encoding plus RocksDB compression) |
| 13 RPC | `protx list/info/diff`, `masternodelist`, `masternode count/winners` | ≤ dashd (`core_order` cached per list, O(1) counts) |

PF1 has not yet published the DML share of `ConnectBlock` (PLAN §8.1), so the numbers below are hypotheses PF1 confirms **before** the payee queue is treated as a lever rather than a parity feature (PLAN EV2b depends on PF1 for this budget).

### 5.2 Where Core spends, and what we remove (n ≈ 3,500 MNs on mainnet, p ≈ 400 decaying; k, the changed entries per block, is ≈ p + 10 ≈ 370–400 while eager decay participates, not 10–20: rev1, GPT F6)

| Step | Core | Rust fast path | Twin |
|---|---|---|---|
| list copy | O(1) + HAMT path copies per mutation | O(1) + path copies in 3 maps / 4 sets | same as fast |
| payee | O(n) scan, plus an O(n) Evo scan between V19 and MN_RR | O(log n) `first()` + a tiny top-run scan | O(n) |
| confirmation | O(n) scan of `prev` | O(c log n) | O(n) |
| decay | O(n) scan + O(p) `UpdateMN`, each copying ~400 B of state, **hashing both BLS keys** (`UpdateUniqueProperty`, v24) and **building two SML entries** when the SML cache is set (`deterministicmns.cpp` v24:589-592) | O(p log n) `update_hot`: a 20-byte `MnHot`, no hashing, no SML work | O(n) + O(p) |
| per QC punishment | O(members) × `CalcMaxPoSePenalty` O(n) (`GetCounts` walks the map) | O(members log n), O(1) counts | O(members × n) |
| collateral spends | SHA256d(36 B) + HAMT lookup per input | one hash-map probe per input | SHA256d + lookup |
| payee bump, streak reset | O(1) + O(n) scan of `newList2` | O(1) + O(s log n), s ≤ 2 | O(n) |
| `BuildDiff` | O(n): iterates `to`, looks up each in `from` | `OrdMap::diff` O(k log n), shared subtrees skipped | O(n) |
| `to_sml` after `ProcessBlock`; the second payee in `GetBlockTxOuts`; the second build in `TestBlockValidity` | O(n) entry construction per block; two more scans; a second build | EV4: O(k) new hashes; the payee once; MN1's memo | — |

### 5.3 Data structures and measured costs

The structures are `imbl` 7 `OrdMap`/`OrdSet` (B-trees of 16-entry chunks with `Arc` sharing) and one `imbl::HashMap` (HAMT, 32-slot nodes). **Measured** with `/work/scratch/ev2-opus/bench-imbl` (a model with the real map shapes, not the real code; `perf stat -e instructions:u`, because wall time on agentbox is meaningless under load), indexed path vs a Core-shaped twin, modes 1 and 2 minus mode 3 (setup):

| Model | Indexed | Twin | Ratio |
|---|---|---|---|
| 3,500 MNs, ~370 decaying, 100 inputs/block (4,000 blocks) | **1.44 M instr/block** | **7.6 M** | **5.3×** |
| 20,000 MNs, ~2,000 decaying (1,000 blocks) | **7.0 M** | **31.8 M** | **4.5×** |

**This is a limited model, not the implementation** (rev1, GPT F6, who reproduced 5.26×). It keeps `unique` in an `OrdMap` keyed by SHA256d (cached per cold state), has two of the four indexes, shares the hot/cold layout with its twin, omits `BuildDiff` encoding and serialization (it counts structural diff items), omits `core_order` materialisation, SML and Merkle work, and its average diff had 368.5 items per block. The pre-image key removes hashing from registrations, key changes and collateral probes, but variable-length SipHash over up to 48 bytes and the collision fixture of §2.2 add cost, so no monotonic claim is made: PF0 measures the complete implementation (§5.5), with cold `core_order`, adversarial property keys, reconstruction, allocations and cache RSS, and reports a build-only lower bound separately from total DML plus consumer work.

- **Clone:** one `Arc` increment per persistent structure (eight) plus the cached order.
- **Update:** per decaying MN a new `Arc<Dmn>` (~130 B) plus a path copy of about three 640 B nodes; `diff_to` ≈ 0.1 µs per changed entry.
- **Memory:** a list's unshared part after a typical block ≈ 0.25 MB (touched `mns` leaves, `decaying` leaves, new `Dmn`s). Cold parts and the unique map (the largest, ≈ 1.2 MB at 3.5 k × 5 properties) are almost always shared. Lists 32 heights apart share almost no `mns` leaves (~0.4 MB each), so the 64-list cache is ≈ 25–50 MB, Core's order of magnitude; the retained 576-block window is ≈ 35 MB worst case (NFR-RES-01).
- **Disk:** the diff bytes equal v24's `dmn_D4` content; per penalised MN per block `VARINT id ‖ VARINT fields ‖ i32` ≈ 7 B; the decay entries dominate (~2.5 KB/block), which is what Q5's lazy decay would remove.
- **Reconstruction:** decoding one snapshot ≈ 3.5 k `try_add`s (no hashing; ≈ 10–20 M instructions); each diff entry ≈ 0.5–1 k instructions, ≈ 0.1–0.2 G for 575 diffs; mini-snapshots bound repeated nearby requests (P6b serving).

### 5.4 Route to 2×, and the declared limit

- **Target (PLAN EV2b BG):** `build_new_list_from_block` ≥ 2× the twin on mainnet blocks, in PF0 instructions; expected 3–5×. **Against dashd:** the twin reproduces Core's algorithmic work, and Core additionally pays `shared_ptr` state copies, SML entries in `UpdateMN`, the second payee computation and the second payload decode; the Core bench patch (§5.5) gives the component-level "never slower than dashd" number.
- **Declared limit: eager PoSe decay.** Every penalised valid MN gets a new state every block, as in Core, so O(p) remains. A lazy representation (`(penalty, height)`, materialised per list height) would remove it and ~95 % of the stored diff bytes, at the price of a representation that encoding, `diff_to`, `pose_punish` and the twin must all materialise. Deferred to the PF track, gated on PF1 (Q5).
- **Budget rule** (A1 §3.10, PLAN): the queue and the four indexes are implemented, the twin is implemented and runs under xcheck in CI replay. If PF1 shows the DML share of connect time below 1 %, the indexes stay as parity-neutral structures, the 2× bench stays the gate for the build, and the twin is never removed (NFR-CEQ-12).

### 5.5 Benches and counters (PF0 `contrib/node/perfgate/workloads.toml`)

| Workload | Fixture | Counter | Gate |
|---|---|---|---|
| `crit:mnlist_clone_diff` | the canonical list at a mainnet 576-multiple near the tip (from `getcanonicalmnlist`); 100 random updates per iteration, then `clone` + `diff_to` | instructions, allocations | never slower |
| `crit:mnlist_unique_probe` (rev1) | 10⁵ `has_unique`/`try_add`/`remove` on the same list with (a) normal keys and (b) the GPT F3 collision fixture (512 colliding IPv6 services), for SipHash (chosen), Fx and `OrdMap` | instructions, max bucket probe length | SipHash ≤ 1.2× Fx on (a); (b) within 2× of (a) for the chosen hasher; recorded once, DEC-50(3) |
| `crit:mnlist_codec` | the same list: encode, decode, hash; a 576-diff window encode/decode | instructions | never slower |
| `crit:dml_build_w1` | mainnet 1,028,160 + 2,000 blocks from the default list | instructions, allocations | never slower; **fast ≥ 2× twin** |
| `crit:dml_build_w2` | 2,000 blocks at tip − 50k from an imported canonical snapshot (QCs present, post-MN_RR) | same | same; a regression beyond max(noise, 0.5 %) blocks a merge (A1 §3.11) |
| `crit:dml_payee` | 10⁴ `payee()` calls on the w2 list, both paths | instructions | fast ≥ 10× twin (sanity) |
| `crit:dml_resolve_576` | `resolve_list` 575 diffs from a snapshot | instructions | never slower |
| `crit:dml_core_order` | `core_order()` on the w2 list | instructions | never slower |
| `reindex-chainstate` windows (PF0's existing four) | — | whole-node counters | regression floor |

**Core side** (DEC-16 test patch, built with B1's script, as CS0's N4): `bench/evo_dml.cpp` runs `RebuildListFromBlock` + `BuildDiff` + diff serialization over the same W1/W2 blocks with the same starting list (from the dump RPC, Q3) and reports instructions per block at both pins.

---

## 6. Verification

### 6.1 L0: Core's own tests

- **`src/test/evo_snapshot_tests.cpp` (v24)**, the MN-list and MN-diff parts (names verified present at the tag): `populated_v3_golden_value` (MN-list section), `canonical_mn_reader_rejects_order_and_counter`, `nested_mn_map_wire_order_is_canonical`, `hash_prefix_collision_runs_are_bounded`, `unset_mn_id_is_rejected`, `inactive_mn_roundtrip`, `malformed_lazy_operator_keys_are_rejected`, `diff_chain_roundtrip_and_canonical_determinism`, `bounded_readers_reject_claimed_sizes_first`. Expected values come from the oracle at test time where the input is generated (CS0's rule), or from Core's committed constants.
- **`src/test/evo_deterministicmns_tests.cpp`.** The DML assertions are ported as scenario tests on the `testkit` chain (synthetic blocks, keys, ProTx builders, `MemEvoStore`). Both pins: `dip3_activation_*`, `v19_activation_*`, `dip3_protx_*` (registration, update, revoke, payee rotation), `field_bit_migration_validation`, `migration_logic_validation` (the legacy diff reader). v24 only: `deterministic_mn_list_diff_serialization_is_canonical`, `FuncProUpServInvalidNType` (EVO-15; run under both profiles with the v23 expectation taken from the v23 oracle), `FuncProUpRegTxVersionHandlingBeforeV24`, and the V24 cases (`migration_rejected_when_key_squatted`, `proupserv_migrates_legacy`, `evonode_raised_to_extaddr_by_service_update_only`, `FuncSameMnSameBlock*`, `FuncPreV24CrossSchemePairCannotBecomeResident`, `FuncSameBlockCrossSchemeKeyPairRejected`, `FuncProRegTxRejectsCrossSchemeKeyReuse`, `FuncProUpRegTxRejectsCrossSchemeKeyReuse`). The mempool cases go to MP1a, `FuncVerifyDB` to CH5.
- **`evo_sharedmn_tests.cpp` (v24)** for the V24 branches; **`block_reward_reallocation_tests`** for payee rotation (shared with EV3).

### 6.2 Unit, property and model tests (every PR)

- **`mnlist_model`** (proptest, also a fuzz target): random sequences of `try_add`, `update`, `update_hot`, `remove`, `pose_punish`, `decrease_scores`, `apply_diff` on `MnList` and on the twin's `RefMnList`; after every step equal canonical bytes, equal payee and projected payees, `check_invariants()`.
- **`mnlist_payee_queue`** (the PLAN's name): payee and projected payees under every `PayeeRules`, on lists with heavy ties, streaks, revivals and all-banned states; the `pay_height` `else if` edge cases; payee banned/removed/revived in the same block; the MN_RR activation block.
- **Codec round trips (rev2, GPT R2-m2: the equality projection is stated per layer).** Raw Core codecs: `encode(decode(b)) == b` or a rejection on arbitrary bytes, no panic, bounded allocation; `decode(encode(x))` equals `x` in **canonical content** (everything `SerializeCanonicalMNList` carries: entries, counter, height, hash), not in bucket history, which the raw bytes do not carry; `apply_diff(a, a.diff_to(b))` equals `b` under Core's `IsEqual` projection and in every surviving internal id, with the counter following Core's `ApplyDiff` rule (the predecessor's counter, bumped by `max(added id + 1, counter)` per addition, §2.8), so it is lower than `b`'s when the diff hides a register-and-spend; every decoder rule of §2.9 with its error text; the `dmn_D3` → `dmn_D4` bit mapping. Our records: `apply_record(a, record(a → b)) == b` and `decode_record(encode_record(x)) == x` in **full equality** (content, counter, ids, `collision_buckets`, `core_order()`).
- **Bulk-order fixtures (rev2, GPT R2-F1):** `quirk_41_bulk_decay`: four crafted MNs with one 64-bit hash inserted 1,2,3,4 (bucket `4 3 2 1`), all penalised and valid, a fifth non-colliding MN holding the minimum pay height; after one block the bucket is `1 2 3 4` and `core_order()`, `get_by_operator_key` on a cross-encoding pair inside the bucket and the encoded trailer equal the twin's and `oracle_mnlist_core_order(ops)`; variants for confirmations (two colliding unconfirmed MNs), streak resets (two colliding Evo MNs), a hot-only no-op update, and the same after cache eviction and `apply_record` reconstruction.
- **Payee fallback fixtures (rev2, GPT R2-F2; rev3, R3-F1/F2):** the two-Evo fixture (height 100, both `lastPaid = 100`, `consecutive` 1 and 2, CHAMP order A, B: payee B, projection `A A A A A A A B`) and the height-0 fixture (a regular MN registered at −2 and an Evo at −1, both valid, `lastPaid = 0`, V19 on, MN_RR off: payee = the Evo); both decode through our canonical reader, trip P1 or P2, and run the literal scan. **Future-revival fixture** (fast path): height 100, `R: regular, lastPaid 1, registered 10`; `E: Evo, lastPaid 100, registered 20, revived 101, consecutive 1`; P1 ∧ P2 hold, the range walk finds E at key `(101, E)`: payee E, projection `E E E R E`. **Future-registration fixture** (fast path; rev4 relabelled, R4-m2): height 0, `R: regular, lastPaid 1, registered 10`; `E: Evo, lastPaid 0, registered 101, consecutive 1`; P1 ∧ P2 both hold (a positive future registration does not break P2), the range finds E: payee E, projection `E E E R E` at both pins. **Negative-registration P2 fixture** (literal): the same list plus a valid regular MN with `lastPaid 0, registered −2` (P2 false at height 0 → `scan.rs`). **Read → build fixture** (`/work/scratch/ev2-gpt3/rebuild_probe.cpp`): predecessor height 100, `A, B: Evo, lastPaid 101, consecutive 1, 2`; `C: regular, lastPaid 1`; build block 101 with no special txs: C is paid, A and B keep `lastPaid 101` with streaks reset, the result fails P1, and `projected_payees(9)` on it is Core's `A A A A A A A A B` (a builder-certified cache would have given `A A A A B B B B C`). **QcTx ordering fixture** (`quirk_41_qctx_order`, R3-m1): four colliding punished members presented in member-vector order `1 2 3 4` on bucket `4 3 2 1` leave `4 3 2 1`; `diff_mnlist_order` compares the ordered projection vector, its length and repeated identities against `oracle_mnlist_payee`/`oracle_mnlist_projected` on random reader-accepted lists with negative registration heights, height 0 and several continuation candidates.
- **`core_order`** against a naive recursive trie on adversarial key sets (shared 5k-bit prefixes for k = 1…12), and the collision-bucket history tests of §2.7 (crafted colliding txids; insert, hot/cold update, replacement, removal, re-registration; `get_by_operator_key` first match).
- **Reconstruction equality (rev1, GPT F2):** register-and-spend in one block; register at `h`, spend at `h+1`; two replacements between snapshots; a trailing registration that disappears while other additions survive. After each: evict every cache, `resolve_list` every affected height, restart (`MemEvoStore` from records), disconnect and reconnect, register again; `total_registered`, internal ids, canonical bytes, `core_order()` and the existing-row rewrite check must equal the online list's.
- **Quirk tests** per §3.12; **unique-key equivalence against Core's hashes** (rev1, Opus m3): `SHA256d(UniqueKey::x(v).bytes()) == GetUniquePropertyHash(v)` on `oracle_unique_property_hash` vectors for IPv4, IPv6, CJDNS, TORv3, I2P, DomainPort, keyID, uint160, legacy and basic BLS keys; the null-value rules of §2.2.
- **Fuzz targets:** `fuzz_canonical_list_decode`, `fuzz_canonical_diff_decode`, `fuzz_s3_decode`, `fuzz_d4_decode`, `fuzz_d3_decode`, `fuzz_state_diff_roundtrip`, `fuzz_apply_diff` (random diffs onto random lists: errors are `MnListError`, never panics); all through the CS0/O3 runner (DEC-30 N5).

### 6.3 L1: differential targets against the oracle

**Two layers, never mixed (rev1, GPT F4).** A *validated-block* target runs Core's whole `ProcessSpecialTxsInBlock` path (the `CheckSpecialTxInner` loop, then the build, then `ProcessBlock`) against CH7a's orchestration of EV1 + EV2b, on arbitrary inputs, per profile, and records **which layer rejects** (`RejectLayer::{Ev1, Ev2b, ProcessBlock}`) with the string; a raw-helper disagreement can therefore never be normalised away. A *raw-helper* target runs the bare `RebuildListFromBlock` against `build_new_list_from_block` on inputs **constrained to the intersection of both pins' reachable domains** (every payload passes EV1's checks against pprev under both profiles; members and `validMembers` inside LQ2's domain), where the one Rust order is correct for both. Known out-of-domain helper differences (§3.10 (d); `MemberError::UnknownLlmqType`) are tested separately as expected differences, never fed to the parity target.

| Target | Layer | Core entry point | Inputs | Compares | Budget |
|---|---|---|---|---|---|
| `diff_dml_block` | validated block | `oracle_dml_block` (O2a-EV: `CheckSpecialTxInner` loop + `BuildNewListFromBlock` + `ProcessBlock`, both shims, profile-selected) | a prev list (canonical bytes: real lists at sampled heights + synthetic 0–5,000 MNs with mixed versions/types/ban states/streaks); a block of 0–50 transactions from a structured generator (every ProTx type and version incl. malformed payloads, duplicates, every quirk 23/42 collision, replacements, QcTx with random members and `validMembers`, ordinary transactions spending collaterals, registered-and-spent); pprev ancestry hashes; coins or `Unchecked`; every V19/MN_RR/V24 flag combination the selected network admits | rejecting layer, verdict class + reason (or the exception class), the new list's canonical bytes, the payee | ≥ 24 CPU-h per profile |
| `diff_dml_apply` | raw helper | `oracle_dml_apply` (O2a-EV; the bare `RebuildListFromBlock`) | as above, filtered to the reachable intersection | verdict/exception class, canonical bytes, payee | ≥ 24 CPU-h on **both** shims with V24 off; ≥ 24 CPU-h on the v24 shim with V24 on |
| `diff_dml_sequences` | validated block, multi-block | `oracle_dml_block` chained | valid sequences with continuation, ban, revive, removal and replacement across V19 and MN_RR, plus restarts (records → `resolve_list`) | indexes, payee and projection after each step and after reconstruction | ≥ 8 CPU-h per profile |
| `diff_dml_apply_window` | raw helper, real blocks | `oracle_dml_apply` with the oracle's list at `h−1` | mainnet blocks (BS2 corpus) from 1,028,160, sampled | canonical bytes per block | until CH7a wires the L2 harness |
| `diff_mnlist_codec` | codec | `oracle_mnlist_canonical(bytes)` → Core's `UnserializeCanonicalMNList` verdict, re-encoding, `CanonicalMNListHash`; also `s3_decode_canonical` and `diff_d4_decode_encode` ops | mutated canonical lists and Core rows | accept/reject, bytes, hash | ≥ 24 CPU-h (v24 shim) |
| `diff_mnlist_diff` | codec | `oracle_mnlist_diff(a, b)` → `BuildDiff` + canonical diff bytes; `oracle_mnlist_apply_diff(a, d)`; `oracle_state_diff(a, b)` | list pairs from `mnlist_model` traces; random state pairs incl. re-encoded keys and netinfo kind changes | diff bytes; applied list bytes or throw | ≥ 24 CPU-h (v24 shim) |
| `diff_mnlist_order` | list | `oracle_mnlist_core_order(ops)` → `ForEachMN` order after replaying an `AddMN`/`UpdateMN`/`RemoveMN` **operation sequence** (so collision buckets are reproducible); `oracle_mnlist_payee(bytes, v19, mn_rr)`; `oracle_mnlist_projected(bytes, n, mn_rr)` (deployment context supplied) | lists with adversarial prefixes and crafted 64-bit collisions; **any** canonical-reader-accepted list for payee/projection (no domain restriction, §3.4) | order, payee, projection | ≥ 24 CPU-h (v24 shim; the code is identical in v23) |
| `diff_unique_hash` | property | `oracle_unique_property_hash(kind, bytes)` → `GetUniquePropertyHash` | every property type and class of §2.2 | hash | 1 CPU-h, plus committed vectors |

**New oracle work ("O2a-EV" follow-up, Q3):**
1. `oracle_dml_apply` exists only in A1's ABI sketch; the shim branch has no DML entry point today. It needs explicit pprev ancestry hashes (for `GetAncestor(quorumHeight)`), a `has_best_block` flag (`Coins` vs `Unchecked`; its synthetic `CCoinsView` must expose a non-null `GetBestBlock()` so the collateral re-check runs), the `v24` flag, and **a test seam that replaces `llmq::utils::GetAllQuorumMembers` with caller-supplied member lists** per `(type, quorumHash)` (a DEC-16 patch in the shim build). Without the seam, QcTx PoSe is fuzzable only through O2b `diff_chain`. It returns the full `oracle_result` and the canonical list bytes.
2. `oracle_dml_block` (the validated-block layer: the check loop, the build and `ProcessBlock` under a selected profile, returning the rejecting stage) and the list entry points above (`oracle_mnlist_canonical`, `oracle_mnlist_diff`, `oracle_mnlist_apply_diff`, `oracle_state_diff`, `oracle_mnlist_core_order(ops)`, `oracle_mnlist_payee`, `oracle_mnlist_projected` with deployment context, `oracle_unique_property_hash`). **These are prerequisites of the consensus merges** (EV2a-3 for the codec and hash targets, EV2b-2 for the block targets), not an eventual follow-up (rev1, GPT F4; charter: differential fuzzing for consensus code).
3. **A hidden RPC on O8's patched v24 node (`node/state-digest-v24.0`): `getcanonicalmnlist <blockhash>` → `{ hex, hash }`**, i.e. `SerializeCanonicalMNList` + `CanonicalMNListHash` (≈ 30 lines, DEC-16). Both legs asked for it under different names (`dumpmnlistcanonical`, `getcanonicalmnlist`); one RPC, this name. EV2a's acceptance ("on imported lists") and the W2 bench start depend on it.

**O2a erratum (review 2026-10-08, item 1 above).** O2a has implemented `oracle_dml_apply`: the member seam, the best-block flag, ancestry hashes and the V24 flag are in, so item 1 is absorbed and O2a-EV no longer needs to build them. Two corrections to what it returns: its output is **evodb bytes, not canonical bytes**, and its input list comes back in **Core's reload order** (not memcmp or the canonical order). The canonical-bytes output moves to O2a-EV (PLAN), and `diff_dml_apply` and `diff_dml_apply_window` compare canonical bytes only once that lands. The harness counts `ORACLE_E_ARG` (the raw helper's broad refusals) as out of domain, never as agreement or divergence, and `oracle_dml_block` runs Core's check loop before any raw-helper precondition, so validated-block verdicts are never pre-empted. DEC-70.

The codec and order targets run on the v24 shim only: v23.1.8 has no `snapshot.*`, and the encodings and iteration code they test are identical at both tags (§2.7, §2.9).

### 6.4 L2: replay windows and digests

- **EV2a gate.** `getcanonicalmnlist` at every 576th block from DIP3 and at the A1 §4.6 boundary heights (1,047,199; 1,088,639; 1,374,911; 1,737,791; 1,899,071; 1,987,775; 2,128,895; tip − 50k), on mainnet and testnet, stored as `test-vectors/evo/mnlist_canonical/<net>/<height>.hex` with the hash. Our decoder reads the bytes, our encoder reproduces them, our hash equals Core's; plus field equality against `protx list registered true <h>` JSON and order equality against its entry order (quirk 41). **1,028,159 (pre-DIP3) is a hash-and-bytes-only fixture** through the unvalidated encoder (§2.9): Core's reader rejects the default list's height −1 (`snapshot.h` v24:256), so no decode round trip is possible there (rev1, Opus m5 / GPT F5). EV4 re-runs the comparison on lists read from a Core `evodb/` at M4.
- **EV2b merge gate (rev1, Opus M1).** Three windows, `CanonicalMNListHash` per block equal to the O8 `-statedigestlog` of the patched v24 oracle (A1 §4.6), with the harness feeding BS2 blocks through `build_new_list_from_block` + `process_block` with `CollateralView::Unchecked` (Core's own null-best-block path, used by `evodb repair`) and a **recorded** `QuorumMemberSource`, so it needs neither CH3a's UTXO set nor LQ1a (DEC-50(7)):
  1. **Mainnet 1,028,160 → 1,128,159** from the default list (no imported state). The Opus reviewer's scan of the oracle's blocks found every QcTx in 1,028,160–1,078,159 to be a null `LLMQ_50_60` commitment, with the first non-null commitments between 1,080,552 and 1,082,963 (43 signers, 43 valid of 50) and one every DKG interval after; the extended window therefore covers the mass DIP3 registrations, ProUpServ/ProUpReg/ProUpRev, replacements, collateral spends, the DIP3 enforcement block, the first PoSe punishments and bans, decay, at least one revive, and the 1,088,639 boundary.
  2. **Mainnet 20k blocks from the 1,899,071 snapshot vector** (inside [V19, MN_RR), with the evonode continuation and streak rules live; if `protx list evo` is empty at that height the start moves to the first 576-multiple at which it is not).
  3. **Mainnet 2,118,528 → 2,138,895** from the 2,118,528 snapshot vector (the 576-multiple before MN_RR at 2,128,896), across the MN_RR switch; the 2,128,895 boundary vector is checked on the way.
  Windows 2 and 3 start from imported canonical snapshots, so they also exercise `decode_canonical_list` + `try_add(bump=false)` as a list source. **Recorded quorum members come from an O7-style replica** (`quorum info <type> <hash>` per mined quorum in the windows; `quorum info` is not a pure read), never from the live oracles (DEC-51 / LQ1 rev1 §6.3.6). The O8 digest node must not restart inside a window (§3.11 item 4); a mismatch confined to `totalRegistered`/`internalId` is reported as oracle provenance. On any other mismatch the harness pulls `protx listdiff h−1 h` around it (digest-first, A1 §4.6 X-2).
- **With CH7a (M4):** full mainnet post-DIP3 and full testnet with real UTXO views and real members; `-consensusxcheck` on at milestone tags (fast vs twin per block; `check_invariants`; every reconstructed list equal to the cached one when both exist; any difference aborts); the segment snapshots of A1 §4.6; the sparse `protx list`/`listdiff`/`masternode payments` goldens.
- **O2b `diff_chain` (M4):** failed connects and proposals touching the DML (a block failing after a ProRegTx; `TestBlockValidity` at a snapshot height; a `ResetPlatformBan` surviving `bad-dip3-enf-block` on a disposable regtest) to confirm nothing persists from a failed build and that effects match step by step.

### 6.5 L3 operators for O10 (GBT proposal mode against both references)

Duplicate platformNodeID in-block; keyID↔platformNodeID in both orders; a duplicate service in-block; a duplicate operator key in-block, pre-V24 and with V24; a collateral created in the same block; a collateral spent by an ordinary tx in the same block as a ProUpServTx for that MN; a replacement ProRegTx; a QcTx with a wrong quorum hash, beyond the tip, or with a negative computed height; a ProUpServTx with out-of-range `nType` (EVO-15 per profile); a ProUpRevTx then a ProUpServTx in one block.

### 6.6 Functional tests (O11/O12, with CH7a)

`feature_dip3_deterministicmns.py`, `feature_dip3_v19.py`, `feature_llmq_simplepose.py` (PoSe), `feature_llmq_evo.py` (four in a row, MN_RR), `feature_masternode_params.py`, `rpc_masternode.py` (winners/payments), `feature_dip4_coinbasemerkleroots.py` (with EV4), `rpc_netinfo.py`, `feature_protx_version.py`, `rpc_protx*.py` (with RP2a), `feature_masternode_shares.py` and `feature_masternode_payout_shares.py` (v24, V24 rules). Mixed mode A at M5, B and all-Rust at M7 (A1 Appendix D). Under `-releaseprofile=23.1`, the V24 parts of v23.1.8's `rpc_netinfo.py` and `feature_asset_locks.py` are the Q1 exceptions.

---

## 7. Implementation plan

Each phase is one reviewable PR (CON class: two reviewers + `code-review` high) with its own tests. EV2a ≈ L (≈ 3 weeks), EV2b ≈ L (≈ 3 weeks), as in the PLAN; EV2b-3 can run in parallel with EV2b-4.

| Phase | Content | Depends on | Acceptance |
|---|---|---|---|
| **EV2a-1** | `types`, `unique` (pre-image keys, SipHash `UniqueMap`, null rules), `list` (`try_add`/`update`/`update_hot`/`remove` with rollback, `collision_buckets`), `index`, `counts`, read API (§2.6 minus `core_order`), `MemEvoStore` + `LruListCache` + `resolve_list` over `MnListDiffRecord` structs (no bytes yet; counter and buckets restored), `testkit` builders | EV0 (or EV2a lands the stubs), CS0, BS6a, BS11a-t types; `oracle_unique_property_hash` vectors | unit tests for every `try_add` failure order and null rule; `mnlist_model` against a minimal twin; quirk 23 tests incl. `diff_unique_hash` against Core's hashes; the reconstruction-equality tests (§6.2); clippy determinism lint clean; **LQ1a can start here** (PLAN lever 2) |
| **EV2a-2** | `core_order` (quirk 41, buckets included), `get_by_operator_key`, `diff_to`/`apply_diff`, `MnStateDiff` | EV2a-1; `oracle_mnlist_core_order(ops)` | `core_order` equals the mainnet and testnet oracles' `protx list` order and a naive trie; the collision-bucket history tests against the oracle op replay; diff round trip; `reference::build_diff_scan` |
| **EV2a-3** | `codec`: canonical list/diff/Dmn/state, `canonical_mn_list_hash` (unvalidated) and the validated storage encoder, `core_disk` readers, record envelope with trailer, `DmlRecordBytes` | EV2a-2; O8's `getcanonicalmnlist`; O2a-EV codec entry points (prerequisite) | L0 `evo_snapshot_tests` subset; codec fuzzers 1 h clean; **imported lists: our bytes = Core's, our hash = Core's** at every 576th block and the §6.4 boundaries on both networks (EV2a's PLAN acceptance; 1,028,159 hash/bytes only); `diff_mnlist_codec`/`_diff`/`_order` ≥ 24 CPU-h |
| **EV2b-1** | `payee`/`projected_payees` (range-walk fast path gated on list-computed P1/P2, `scan.rs` literal fallback in every build), `pose`, confirmations, bookkeeping, streaks; the full reference twin behind `xcheck`; `Provenance` and `check_invariants` wiring (I-U, I-X, I-C; I-P asserted only on `FromGenesis` lists, P1 compared with a literal count on `Decoded` ones; no panic on any decoded-data path) | EV2a-2 | `mnlist_payee_queue` 10⁷ ops clean incl. I-P-breaking, future-revival, height-0 and read → build lists vs `oracle_mnlist_payee`/`oracle_mnlist_projected`; **a default-feature (no `xcheck`, no `test-utils`) build and `cargo doc` pass in CI**; quirks 13 and 24; the L0 payee scenarios; `crit:dml_payee` ≥ 10× twin |
| **EV2b-2** | `build_new_list_from_block` pre-V24 (3a–3d, 3f, step 4) with EVO-15's reachable order; `set_state_version`/`apply_pro_up_serv` pre-V24 subset; `process_block`/`undo_block` (counter and bucket trailers); error mapping incl. `MemberError::Store` → node error; effects; `DmlTrace` | EV2b-1, EV1 (scenario tests), EV0's `QuorumMemberSource`, O2a-EV block entry points (prerequisite), the O7-style replica for recorded members | `diff_dml_block` ≥ 24 CPU-h per profile and `diff_dml_apply` (both shims, V24 off) ≥ 24 CPU-h; `diff_dml_sequences`; **L2: the three §6.4 windows digest-equal (1,028,160–1,128,159; 20k from 1,899,071; 2,118,528–2,138,895)**; quirks 25, 26 (DML half), 42; L0 `evo_deterministicmns_tests` (both pins); the no-`UngatedRules` test; the layer-attributed EVO-15 tests of §3.10; reject-string census |
| **EV2b-3** | V24: 3e, `set_state_version` full, the cross-scheme probes, the shared spend rule | EV2b-2, BS6c (v3 payloads), BS11a-t `reencode` | v24 L0 V24 cases and `evo_sharedmn_tests`; `diff_dml_apply` (v24 shim, V24 on) ≥ 24 CPU-h; a regtest V24 window digest (testnet once V24 activates there) |
| **EV2b-4** | benchmark gate: criterion/iai benches, PF0 workloads, the Core `bench/evo_dml.cpp` patch, `-consensusxcheck` wiring points documented for CH7a | EV2b-2, PF1 budget | `crit:dml_build_w1`/`w2` ≥ 2× the twin (instructions); ≤ dashd's DML instructions per block; baselines recorded |

---

## 8. Open questions for the manager (deduplicated across both legs; items 1–12 were decided in DEC-50 and are kept for the record; 13–14 are new in rev1)

13. **Collision-bucket parity target.** Rev1 reproduces the from-genesis online 64-bit order and records three Core inconsistencies (reconstructed lists, imported datadirs, 32-bit hosts). Since Core nodes disagree among themselves inside a bucket, O14's `protx list` comparison and EV4's `mnlistdiff` byte comparison against a *restarted* oracle can differ from ours inside a bucket only. *Recommendation:* accept the online target as the parity definition for quirk 41, and have O14/EV4 treat a within-bucket permutation against a restarted oracle as oracle provenance (no such bucket exists on mainnet or testnet today).
14. **Oracle provenance for `totalRegistered`.** The O8 digest node's `CanonicalMNListHash` depends on whether it restarted across a register-and-spend block (§3.11 item 4). *Recommendation:* O8 runs the digest from genesis without restarts inside the compared windows, and the digest log records restarts so the harness can classify a counter-only mismatch.

1. **v23.1.8's own V24 rules on regtest.** v23.1.8's `rpc_netinfo.py` and `feature_asset_locks.py` activate v23's V24 (bit 12) on regtest; v23 has different DML rules for it (EVO-11, EVO-14). Under `-releaseprofile=23.1` rdashd would apply v24's V24 rules. *Recommendation:* declare it a regtest-only deviation (mainnet has V24 NEVER_ACTIVE under both pins); O11 puts those tests' V24 parts on the 23.1 expected-failures list; do not emulate a dead rule set.
2. **Ownership of `process_block`/`undo_block`** (diff, snapshot cadence, `ResetPlatformBan`, `bad-dip3-enf-block`). Fable had CH7a emit the effect and `make_delta` hold the enforcement check; Opus had EV2b own all of it. *Recommendation:* EV2b owns `process_block` including the effect emission (§3.8), because A1 §2.10.1 wants effects emitted by the pure function in Core's position and `diff_chain` compares them at the same step; CH7a owns only the `!just_check` and DIP3 gates and the mapping of `Exception` → `failed-procspectxsinblock`. Confirm.
3. **DEC-16 and oracle scope ("O2a-EV").** (a) O8 adds `getcanonicalmnlist` to `node/state-digest-v24.0`; (b) an O2a follow-up adds the extended `oracle_dml_apply` (member seam, `has_best_block`, ancestry hashes) and the seven list entry points of §6.3. *Recommendation:* yes to both, as a small follow-up like O1b was for CS1; EV2a-3's gate needs (a), EV2b-2's L1 gate needs (b). Also: record once in PF0 whether `UniqueMap` as `imbl::HashMap` or `imbl::OrdMap` is faster; the design picks HashMap on the analysis in §2.2, and the swap is one line.
4. **Register quirks 41 and 42** with owners EV2a and EV2b, and EV4/RP2a/O14/AM1/MP1a as consumers of 41; O5 adds the `#[ignore]` stubs. *Recommendation:* yes. Also accept the A1 §2.5 erratum (iteration order) and the A1 §2.10.1 erratum (`ResetPlatformBan` carries `height`).
5. **Lazy PoSe decay.** *Recommendation:* defer; make it a PF item only if PF1 shows the DML above 5 % of post-DIP3 connect instructions after EV2b's indexes, or the mn CFs exceed dashd's evodb share.
6. **`imbl = "=7.0.2"`, exact pin** (it contains `unsafe` through `imbl-sized-chunks`; a version bump could change iteration or diff behaviour). *Recommendation:* accept, as A1 chose `imbl`; record it in the `cargo deny`/vet notes; treat a bump as a CON change that re-runs the model fuzz and the L1 targets.
7. **EV2b's L2 harness** (§6.4) uses Core's null-best-block path and recorded quorum members, so it waits for neither CH3a nor LQ1a; collateral checks are covered by L1, unit tests and CH7a's full replay. *Recommendation:* approve; add `i:O8` and `i:LQ1a` (trait only) to EV2b's PLAN row, `i:O8` to EV2a's.
8. **A tip snapshot written at each flush**, a derived record used only when `meta.evo_tip` matches, cutting startup from ≤ 575 diff applications to one decode (metric 9). *Recommendation:* yes, owned by CH7a, as a non-consensus record outside `mn_snapshot`.
9. **`CoreBytes` vs DML records.** CS0 §4.5 seals `CoreBytes` to transactions, headers and blocks; the DESIGNER brief says "`CoreBytes` for any bytes that are stored". *Recommendation:* keep CS0's seal; `dml::codec` mints its own `DmlRecordBytes` with the same construction rule (§2.9), and the brief's sentence is read as "a sealed bytes type". If the manager prefers one type, CS0 adds a sealed constructor for `dash-evo`'s codec module.
10. **`EvoStore` and `QuorumMemberSource` ownership and shape** (decided, DEC-50(10)). EV2a lands the DML slice as `DmlReader` plus `MemEvoStore` stubs for the EV3/LQ1b/LQ2 items, with `Result<Option<Arc<_>>>` returns (fail-stop store errors; A1 Appendix A erratum). The member-source trait is **one trait owned by EV0 whose text is LQ1 §2.6 (rev3)** (`QuorumMemberSource::members(t, base, &mut dyn EffectSink) -> Result<Arc<[Arc<Dmn>]>, MemberError>`; `MemberError::Store` a node error, `UnknownLlmqType` → `failed-procspectxsinblock`), reproduced in §2.11 (rev1, Opus m6). The EV2 and LQ1 implementers review EV0's PR together. Note for the manager: DEC-50(10) attributes the ruling to "DEC-48"; the ruling is the LQ1 design review M1 and has no DEC row of its own.
11. **Twin retention after PF1.** If PF1 finds the DML share negligible, keep `cfg(xcheck)` twins for all scans? *Recommendation:* yes (NFR-CEQ-12, A1 §3.10); the twin bench gate may relax from 2× to "not slower" for the sub-millisecond steps, never the build.
12. **`MnListChanged` only-on-change (v23) vs always (v24).** *Recommendation:* CH7a/O17 decide whether a process-internal listener difference needs a profile row; EV2b provides `MnListChange` always. Superseded by the EV0 erratum below: v24 notifies only on changes too, which CH7 D15 follows.

**EV0 erratum (API review 2026-10-09), §3.8 row 6.** v24.0.0-rc.3 fills the list-changed update unconditionally but notifies only when the diff has changes (`validation.cpp:2918`), the same as v23.1.8. No per-profile row is needed for the notification condition (DEC-50(12) stands without a profile split). The notification also has an undo direction (VerifyDB level 3); see the A1 erratum of the same date.

## 9. Proposed errata

- **A1 §2.5:** "O's sorted maps, so that iteration is in Core's order" → Core's order is immer CHAMP order (quirk 41); memcmp order is the canonical encoding's and the SML root's. `MnList.height: u32` → `i32`. The `Dmn`/`MnState` sketch → the hot/cold split with the SML-hash cache per cold state and validity. Add quirk 41's consumers.
- **A1 §2.10.1:** `Effect::ResetPlatformBan { pro_tx_hash }` → `{ pro_tx_hash, height }`.
- **A1 §2.9:** `mn_snapshot` is written every 576 blocks **and at the first DIP3 block**; `mn_diff` is the canonical diff (== v24 `dmn_D4` bytes) in our envelope.
- **A1 Appendix A:** `QuorumMemberSource` is replaced by EV0's trait with the text of LQ1 §2.6 (rev3) (`&mut dyn EffectSink`, `Result<Arc<[Arc<Dmn>]>, MemberError>`); `EvoStore`'s DML methods return `Result`; store errors are node errors everywhere (I5), never `failed-procspectxsinblock`.
- **A1 §2.9 / §3.5 (rev1):** `mn_diff` records carry the post-block registration counter and collision-bucket trailer; a reconstructed list equals the online list byte for byte, which Core's own evodb does not guarantee.
- **Memory note `dash-mnlist-champ-order`:** already corrected to newest-first buckets and the 64-bit parity target; add "a hot-only `UpdateMN` moves the entry to the bucket front", and the reconstruction/import inconsistency.
- **A1 §3.6 lever list:** add the per-update unique-key hashing, the per-update SML-entry construction and the per-input collateral hashing that Core does and we avoid; "payee O(log n)" is a minor part of the gain.
- **O17 EVO-15:** no consumer change (Opus's proposed "add EV2b" is withdrawn: EV2b implements the reachable order without reading the profile, §3.10).
- **PLAN EV2a/EV2b rows:** EV2a acceptance needs `i:O8` (`getcanonicalmnlist`); EV2b adds `i:LQ1a` (trait) and `i:O8`; EV2b's L1 needs the O2a-EV follow-up (Q3).
- **R4 §1.15:** add quirks 41 and 42 (Q4).

## Appendix A: Core line map

| Topic | v24.0.0-rc.3 | v23.1.8 |
|---|---|---|
| `CDeterministicMN` and serialization | `evo/deterministicmns.h:43-94` | `:43-94` |
| immer maps, `ImmerHasher`, `Counts`, fields | `:136-175` | `:135-174` |
| list `Serialize`/`Unserialize` (`dmn_S3`) | `:235-255` | `:234-255` |
| `GetCounts`, `ForEachMN`, `ForEachMNShared` | `:269-324` | `:268-323` |
| `GetHeightForSnapshotCodec`, `SetHeight` | `:340-345` | — |
| `HasOperatorKeyUnderAnyScheme` | `:471-482` | — |
| `IsEqual`, `operator==` | `:487-525`, `:616-624` | `:449-487` |
| `GetUniquePropertyHash`, `Add/Delete/UpdateUniqueProperty` | `:526-614` | `:488-567` |
| `CDeterministicMNListDiff` codec (`dmn_D4`) | `:627-712` | `:580-660` |
| `DISK_SNAPSHOT_PERIOD`, cache sizes, `MAX_CACHED_LISTS` | `:760-769` | `:681-684` |
| evodb keys `dmn_S3`/`dmn_D4`/`dmn_D3` | `evo/deterministicmns.cpp:30-33` | `:33-36` |
| `GetMNByOperatorKey`, `GetMNByCollateral`, `GetMNByService`, `GetMNByInternalId` | `:93-130` | `:119-155` |
| `CompareByLastPaid(_GetHeight)`, `GetMNPayee` | `:132-194` | `:157-219` |
| `GetProjectedMNPayees` | `:196-249` | `:221-274` |
| PoSe | `:267-334` | `:292-359` |
| `BuildDiff`, `ApplyDiff` | `:336-393` | `:361-418` |
| `AddMN`, `UpdateMN`, `RemoveMN` | `:439-679` | `:420-647` |
| `CDeterministicMNManager::ProcessBlock` (ResetPlatformBan loop; enforcement check) | `:689-782` (`:735-750`; `:769-776`) | `:649-734` (`:673-688`; `:722-729`) |
| `UndoBlock` | `:784-817` | `:736-769` |
| `GetListForBlockInternal` (initial snapshot; mini-snapshots), `CleanupCache` | `:826-939` (`:881-889`; `:899-917`), `:975-1014` | `:778-870`, `:906-947` |
| `CDeterministicMNState` fields, codec, helpers | `evo/dmnstate.h:33-175` | `:33-157` |
| `CDeterministicMNStateDiff` (bits; ctor; serialize) | `:177-324` (`:180-204`; `:250-281`; `:288-314`) | `:157-287` (`:160-178`) |
| legacy diff | `:327-483` | `:288-446` |
| `SetStateVersion`, `ApplyProUpServTx`, `IsSchemeMigration` | `evo/specialtxman.cpp:46-137` | inline `:350-367`, `:396-404` |
| `HandleQuorumCommitment` | `:283-298` | `:159-174` |
| `RebuildListFromBlock` (ProReg, ProUpServ, ProUpReg, ProUpRev, shares, QcTx, spends, payee, streaks) | `:309-795` (`:358-453`, `:454-521`, `:522-586`, `:587-618`, `:619-701`, `:702-728`, `:731-757`, `:759-775`, `:777-790`) | `:185-520` (`:235-312`, `:313-383`, `:384-413`, `:414-434`, —, `:435-461`, `:464-482`, `:486-500`, `:503-515`) |
| `ProcessSpecialTxsInBlock`: check loop, DML calls, `fJustCheck` gate, catches | `:862-875`, `:907-916`, `:913`, `:989-996` | `:610-630`, `:628`, `:701-704` |
| `SpecialTxRules.v24` source | `evo/specialtxman.h:49-53`; `validation.cpp:2659` | `is_v24_deployed`, `evo/specialtxman.cpp:224` |
| canonical MN-list and diff codecs, hash | `evo/snapshot.h:158-394`; `evo/snapshot.cpp:93-155` | — |
| `EvoDbInconsistencyError`, `WriteDerived` | `evo/evodb.h:45`, `:150-205` | `Write`, `:78-83` |
| `BuildSimplifiedDiff` | `evo/smldiff.cpp:121-145` | `:122-146` |
| SML sort and entry | `evo/simplifiedmns.cpp:73-79`; `evo/deterministicmns.cpp:43-50` | same |
| `protx list`, `masternodelist` loops | `rpc/evo.cpp:1709, 1740`; `rpc/masternode.cpp:666` | `rpc/evo.cpp:1476, 1506`; `rpc/masternode.cpp:612` |
| `ResetPlatformBan`, `SetPlatformBan` | `masternode/meta.cpp:187-194`; `masternode/meta.h:73-84` | same |
| immer `default_bits`, CHAMP iterator, collision insert/replace/remove, `do_add`/`do_sub`, `hash_t` | `src/immer/immer/config.hpp:126`; `detail/hamts/champ_iterator.hpp`; `detail/hamts/node.hpp:384-405, 433-460, 484-514`; `detail/hamts/champ.hpp:565-576, 1199-1240`; `detail/hamts/bits.hpp:23` | identical |
| `nMasternodeMinimumConfirmations`, DIP3 heights and hash | `chainparams.cpp:177, 185-187, 386, 394-396, 569, 576-578, 810, 818-821` | same values |
| MN type table | `evo/dmn_types.h:30-43` | same |
| member selection consumers | `llmq/utils.cpp:343-466`, `:634-730` (`:458-463` snapshot store) | `:339-420`, `:569` |
| payments consumer | `masternode/payments.cpp:152-157` | `:55-60` |

## Reconciliation log

One line per disagreement: what each leg said, what was chosen, the Core evidence.

0. **Rev1–rev3 note.** Items 1, 4 and 7 below record the r0 decisions; later revisions refined them per the reviews: collision buckets are reproduced from mutation history rather than sorted, with bulk phases applied in Core's phase-start order (§2.7, GPT F1/R2-F1); the replay window grew to 1,128,159 plus two imported-snapshot windows (§6.4, Opus M1); and item 4's "top run" derivation of the continuation candidate became the `pay_height ≥ list.height` range walk gated on list-computed P1/P2 with a literal fallback (§3.4, GPT R2-F2/R3-F1/R3-F2).
1. **Iteration order.** Fable: HAMT order never leaks into a consensus result; only `dmn_S3` (import-only) depends on it. Opus: Core iterates in immer CHAMP order over `ReadLE64(proTxHash)`, observable in mnlistdiff/qrinfo bytes, `protx list`, `masternodelist`, `GetMNByOperatorKey`. **Chosen: Opus.** `ForEachMN` is a range-for over `immer::map` (`deterministicmns.h` v24:301-324 / v23:300-323) with `ImmerHasher = ReadLE64` (v24:136-139 / v23:135-138); `BuildSimplifiedDiff` builds `mnList` and `deletedMNs` with it (`smldiff.cpp` v24:121-145 / v23:122-146); `protx list` (`rpc/evo.cpp` v24:1709,1740 / v23:1476,1506), `masternodelist` (`rpc/masternode.cpp` v24:666 / v23:612), `GetMNByOperatorKey` (`deterministicmns.cpp` v24:93-102 / v23:119-127). Re-verified by the reconciler against the mainnet oracle (4,753 MNs, 454 added, 467 deleted) and testnet (542): `/work/scratch/ev2-rec/*.log`. Fable's "consensus results are order-free" is kept as the list of what does *not* depend on it (§2.7).
2. **Unique-property map key.** Fable: the serialization pre-image, no SHA256d. Opus: the SHA256d hash with refcounts (both had refcounts). **Chosen: Fable's pre-image key, Opus's refcount slot.** `mnUniquePropertyMap` is only probed, compared and snapshotted (`deterministicmns.h` v24:443, 448, 492, 558-585, 623; `deterministicmns.cpp` v24:452, 525, 621; `git grep` at both tags finds no other use), never iterated or serialized, so any injective image of the serialization has identical semantics on every collision class (quirk 23: 20-byte keyID/platformNodeID/CJDNS, 36-byte outpoint/TORv3, per-encoding 48-byte BLS); the pre-image saves a SHA256d per probe and per input (§2.2). Structure: `imbl::HashMap` with a fixed hasher behind a non-iterable newtype (A1 §2.5's own sketch) over Opus's `OrdMap`, for cheaper mutation; PF0 records both (Q3).
3. **Diff and snapshot format.** Fable: Core's `dmn_D4` bytes inside our envelope. Opus: v24 canonical encodings byte for byte, snapshot every 576 blocks plus the first DIP3 block. **Chosen: Opus's canonical encodings and reader rules; the disagreement is empty in bytes.** `BuildDiff` sorts `addedMNs` by `internalId` at both pins (`deterministicmns.cpp` v24:362-366 / v23:388-391) and internal ids are unique, so the canonical `(internalId, proTxHash)` sort is the same order; v24's `dmn_D4` writer sorts `updatedMNs` (`deterministicmns.h` v24:639-659) where v23's does not (v23:591-605). Hence v24 `dmn_D4` == canonical diff for every per-block diff. Snapshot cadence agreed by both: `height % 576 == 0` (`DISK_SNAPSHOT_PERIOD`, `deterministicmns.h` v24:760 / v23:681) plus the first DIP3 block, which Core writes only when `m_initial_snapshot_index` happens to be set (`deterministicmns.cpp` v24:722 / v23:690). Justified by DEC-11 (own formats, import-only Core bytes), A1 §2.9 (which already names the canonical list for `mn_snapshot`) and the PLAN EV2a `CanonicalMNListHash` target (`snapshot.cpp` v24:150-155), which needs the list codec regardless.
4. **Hot/cold split and indexes.** Opus: hot/cold `Dmn`, five indexes (payee queue, decaying, unconfirmed, evo streak, typed collateral mirror) plus a cached `core_order`; 1.44 M vs 7.6 M instruction model. Fable: single `Arc<MnState>`, six derived indexes (payee queue, `last_payee`, unconfirmed, penalised, streaks, counts). **Chosen: Opus's hot/cold split and four indexes plus counts.** Hot/cold because every per-block update Core makes copies a ~400-byte state (`dmnstate.h` v24:33-175) and v24's `UpdateMN` additionally hashes both BLS keys and builds two SML entries (`deterministicmns.cpp` v24:589-592; `deterministicmns.h` v24:596-600); the five hot fields are exactly those decay, bookkeeping and PoSe touch. The typed collateral mirror is dropped because the pre-image key makes `unique.get(outpoint)` Core's own `GetMNByCollateral` (`deterministicmns.cpp` v24:104-107) at one probe; Fable's `last_payee` is dropped because the MN with `nLastPaidHeight == nHeight` sits in the queue's top run (`CompareByLastPaid_GetHeight` v24:132-141; `GetMNPayee` v24:166-181) and I-P makes it unique. One reference twin (`reference.rs`) and one `check_invariants()` (Fable's `debug_assert_indexes` + Opus's I-U/I-X/I-P) are kept.
5. **Effects and errata.** Opus: `ResetPlatformBan` carries a height and is emitted before `bad-dip3-enf-block`, from EV2b's `process_block`. Fable: CH7a emits the effect; `bad-dip3-enf-block` lives in EV2b's `make_delta`. **Chosen: Opus's placement and height; Fable's observation that `ProcessBlock` runs only when `!fJustCheck` becomes CH7a's gate.** `ResetPlatformBan(proTxHash, nHeight)` → `SetPlatformBan(false, height)` (`masternode/meta.cpp:187-194`, `meta.h:73-84`, both pins), called inside `ProcessBlock` before the enforcement check at both pins (`deterministicmns.cpp` v24:735-750 then :769-776; v23:673-688 then :722-729); `ProcessBlock` is gated by `!fJustCheck` in `ProcessSpecialTxsInBlock` (`specialtxman.cpp` v24:913 / v23:628). A1 §2.10.1's rule (pure functions emit through `&mut dyn EffectSink`) is satisfied by threading the sink to `process_block`. Ownership table §3.8. Manager addendum (LQ1 review M1): one `QuorumMemberSource` trait owned by EV0 in LQ1's shape, snapshot effects pass through EV2b's sink, and store faults are node errors (`specialtxman.cpp` v24:989-992 `EvoDbInconsistencyError` → `state.Error`), never `failed-procspectxsinblock`; Opus §3.8's `MemberError` → `failed-procspectxsinblock` mapping is withdrawn.
6. **Quirk proposals.** Opus: 41 (CHAMP order) and 42 (order-dependent in-block exceptions). Fable: "26". **Chosen: 41 and 42.** R4 §1.15 numbers 1–34, rev1 added 35–38 (PLAN O5), CS1 registered 39 (DEC-25), CS2 registered 40 (DEC-29); Fable's 26 is R4's existing "external collateral must be in the pprev UTXO set" (R4 §1.15 item 26; `validation.cpp` v24:2661-2669), listed among its inputs, not a proposal. 41's evidence is item 1; 42's is the asymmetric in-block re-checks (`specialtxman.cpp` v24:403-427 re-check netinfo/owner/operator but not platformNodeID; v24:463-477 netinfo only) against `AddMN`/`UpdateMN`'s full checks (`deterministicmns.cpp` v24:454-507, 527-582) and the catch at v24:993-996.
7. **Oracle and verification needs.** Fable: `dumpmnlistcanonical` on O8, `oracle_mnlist_codec` and `oracle_state_diff` on O2a, `GetBestBlock()` non-null on `oracle_dml_apply`, 50k-block window from 1,028,160, `diff_dml_apply` ≥ 24 CPU-h per release. Opus: `getcanonicalmnlist` on O8, an "O2a-EV" follow-up with the member seam and `has_best_block`, four list entry points, the 1,028,160–1,078,159 digest window with recorded members and `Unchecked` collateral, ≥ 24 CPU-h on both shims plus ≥ 24 CPU-h with V24 on. **Chosen: the union under one name each** (§6.3–6.4): `getcanonicalmnlist` (one RPC), `oracle_dml_apply` extended (ancestry hashes, `has_best_block` so the collateral re-check at `specialtxman.cpp` v24:378-387 runs, `v24` flag, the `GetAllQuorumMembers` seam), the seven list entry points, `diff_dml_apply` ≥ 24 CPU-h on both shims with V24 off plus ≥ 24 CPU-h on v24 with V24 on, the EV2b window with Core's null-best-block path (v24:378) and recorded members, and the EV2a vector gate at every 576th block and the A1 §4.6 boundaries.
8. **EVO-15 handling (found while merging).** Opus: EV2b reads `UngatedRules.proupserv_type_in_special_tx` and O17 should add EV2b as a consumer. Fable: EV2b implements v23's reachable order once and never reads the profile. **Chosen: Fable.** Under `V24_0`, `CheckProUpServTx` rejects out-of-range and mismatched `nType` (`providertx.cpp` v24:420-422; `specialtxman.cpp` v24:1344-1350) in the check loop (v24:862-875) before `BuildNewListFromBlock` (v24:907), so v24's early `bad-protx-type` in the build (v24:459-461) is dead code; v23's order (v23:319-344) is the only reachable one. No O17 consumer change.
9. **`CoreBytes` for DML records (found while merging).** Fable: `canonical_bytes() -> CoreBytes`. Opus: CS0 seals `CoreBytes` to tx/header/block; DML records use crate-private encoders. **Chosen: Opus**, per CS0 §4.5 (sealed trait, `compile_fail` tests); a `DmlRecordBytes` newtype gives the same guarantee; Q9 asks CS0 to confirm.
10. **`ApplyDiffForSnapshot` (found while merging).** Fable listed it in the API; Opus excluded it. **Chosen: excluded** (A1 NG-6: assumeutxo out of scope; v24 `deterministicmns.cpp:395-437` has no v23 counterpart and is only reached from the `EvoSnapshot` container).
11. **Payload decoding (found while merging).** Fable: EV2 decodes through `Tx::payload(bls)`. Opus: CH7a decodes once into `BlockPayloads` shared with EV1. **Chosen: Opus**; Core decodes twice (`CheckSpecialTxInner` and `RebuildListFromBlock`) and decoding is a pure function of the bytes and `UngatedRules.protx_payload_decode` (EVO-14), so sharing is equivalent and halves the work.

## Dispositions of the r1 review findings

| Finding | Disposition |
|---|---|
| **GPT F1** (blocker) collision-node order is mutation history; sorting changes `GetMNByOperatorKey` | **Accepted (ruling 1).** Newest-first and move-to-front verified in immer at both pins; `collision_buckets` added to `MnList`, maintained at the choke point, carried in record trailers; parity target = from-genesis online 64-bit history; reconstructed/imported Core lists and 32-bit hosts recorded as Core inconsistencies; `oracle_mnlist_core_order(ops)` replays operation histories (§2.4, §2.7, §2.9, §3.11, §6.3). Q13 asks the manager to confirm the online target as quirk 41's parity definition |
| **GPT F2** (major) stored diffs lose the counter after register-and-spend | **Accepted (ruling 2).** `MnListDiffRecord.total_registered_after` restored by `apply_record`; `DmlDelta` carries the record; tests for same-block and adjacent-block register-and-spend, replacements, restart, reorg, next registration; oracle-provenance caveat for the O8 digest (§2.8–2.10, §3.11 item 4, §6.2, Q14) |
| **GPT F3** (major) fixed FxHasher admits cheap collision families | **Accepted (ruling 3).** `UniqueMap` = `imbl::GenericHashMap` with `RandomState` (SipHash-1-3, per-process key); pre-image key kept (DEC-50); PF0 `crit:mnlist_unique_probe` on normal keys and the reviewer's fixture vs Fx and `OrdMap` (§2.1, §2.2, §5.5) |
| **GPT F4** (major) raw-helper inputs mixed with post-EV1 claims; wrong EVO-15 example | **Accepted (ruling 4).** Two layers with the rejecting layer recorded; raw-helper inputs constrained to the reachable intersection; expected out-of-domain differences tested separately; EVO-15 vectors corrected and layer-attributed; `oracle_mnlist_projected` gets `mn_rr`; payee has no domain restriction; O2a-EV entry points are merge prerequisites (§3.10, §6.3, §7) |
| **GPT F5** (minor) default-list codec contract vs the pre-DIP3 gate | **Accepted.** Unvalidated hashing encoder vs validated storage encoder; 1,028,159 is hash/bytes-only; I-P is not a reader rule (§2.9, §6.4) |
| **GPT F6** (minor) the 5.3× model does not establish the final gain | **Accepted.** Labelled a limited model; effective k ≈ 370–400 with eager decay; monotonic-gain sentence removed; PF0 measures the full implementation incl. cold `core_order`, adversarial keys, reconstruction, RSS (§5.2–5.3) |
| GPT "surfaces checked": new cold state starts with empty derived caches; CH7a preserves v24's write-check-before-reset ordering; crash tests compare reconstructed historical lists | **Accepted as tests:** added to §6.2 (derived caches) implicitly through `check_invariants`, and to §4's crash tests ("reconstructed historical lists as well as the tip"); the WriteDerived-before-reset ordering is CH7a's and is already in §3.8's step table |
| **Opus M1** (major) the replay window has no non-null commitment | **Accepted (ruling 5).** Window to 1,128,159; two imported-snapshot windows (1,899,071; 2,118,528–2,138,895 across MN_RR); members recorded on an O7-style replica; the conditional sentence replaced by the measured fact (§6.4, §7) |
| **Opus m1** I-P as a decoder rule; `payee()` on lists that break it | **Accepted (ruling 6); superseded twice:** rev2 replaced the rev1 last-match rule by the P1/P2-gated fast path with a literal fallback (R2-F2), and rev3 fixed the fast path's candidate search to the unbounded range `pay_height ≥ list.height` with Core's exact predicates and made P1/P2 list-computed (R3-F1/F2). Decoder rule dropped; I-P post-build only (§2.4, §3.4) |
| **Opus m2** EVO-15 test vector wrong under `V24_0` | **Accepted.** `bad-protx-type` from EV1 under `V24_0`, `bad-protx-hash` from EV1 under `V23_1`; the replacement-then-ProUpServ block reaches EV2b's `bad-protx-hash` under both (§3.10) |
| **Opus m3** tautological unique-key test | **Accepted (ruling 6).** `SHA256d(UniqueKey) == GetUniquePropertyHash` against `oracle_unique_property_hash` vectors for every class (§2.2, §6.2, §6.3) |
| **Opus m4** null-value semantics incomplete | **Accepted (ruling 6).** Per-property null rules for add, delete and update, with Core's error texts and a test (§2.2) |
| **Opus m5** EV2a gate cannot pass at 1,028,159 | **Accepted (ruling 6).** Same fix as GPT F5 |
| **Opus m6** `QuorumMemberSource` shape disagrees with LQ1 | **Accepted (ruling 6).** §2.11 reproduces LQ1 rev1 §2.6 verbatim in shape (`Result<Arc<[Arc<Dmn>]>, MemberError>`); mapping table adopted; the DEC-50(10) "DEC-48" citation flagged to the manager (Q10) |
| **Opus n1** quirk-41 wording (newest-first; host word size) | **Accepted.** Folded into §2.7 and the quirk 41 row; the memory note is already corrected |
| **Opus n2** raw `nType` in the shared-collateral spend check | **Accepted.** §3.5 copies the raw test for fuzz parity |
| **Opus n3** `get_by_operator_key` with an invalid or default query | **Accepted.** Behaviour specified (first MN with a null key); AM1 guards the query (§2.6) |
| **Opus n4** step 9's "duplicate collateral the same MN" cannot happen | **Accepted.** Reworded; cross-type removal noted under quirk 23/25 (§3.2.1) |
| **Opus n5** §3.9/§3.11 overstate v24's node-error handling | **Accepted.** Only `EvoDbInconsistencyError` (credit pool, MNHF) takes `state.Error`; the DML's own faults are `failed-procspectxsinblock` at both pins; the ruling stands, the parity claim is corrected (§2.11, §3.9, §3.11) |
| **Opus n6** fixed-key FxHasher | **Accepted.** Same fix as GPT F3 |
| **Opus n7** `projected_payees(n: usize)` cannot express `nCount < 0` | **Accepted.** Noted as RP2a's argument check (§2.6) |

### Round 2 (GPT)

| Finding | Disposition |
|---|---|
| **R2-F1** (blocker) indexed bulk loops produce a collision order that differs from Core's online history | **Accepted (ruling 1).** Core snapshots each bulk phase in CHAMP order before mutating (`specialtxman.cpp` v23:205-218 / v24:330-343 iterate `prevList`; `deterministicmns.cpp` v23:334-350 / v24:309-326 collect `toDecrease` first; `specialtxman.cpp` v23:502-515 / v24:777-790 iterate the copy `newList2`), so Core's sequence is the phase-start `core_order()` filtered to the eligible MNs; EV2b applies index-found candidates in that order, which reduces to the phase-start bucket sequence inside each collision bucket (§2.7, §3.2). Twin applies literally; I-C compares buckets, `core_order()` and operator first matches after every block, eviction and reconstruction; `quirk_41_bulk_decay` and its confirmation/streak/no-op variants (§6.2). Trailer captures order-only bucket changes; `apply_record` clears caches (§2.8, §2.9) |
| **R2-F2** (major) payee/projection still differ from Core on reader-accepted lists | **Accepted (ruling 2); "built lists satisfy both by construction" superseded by R3-F2/R4-m1.** Fast path gated on checked P1 (I-P) and P2 (`height > 0` or no negative `registered_height`); every list computes `fast_payee_ok` lazily from itself; violation → Core's literal `payee_scan`/`projected_scan`, compiled in every build. The first-scan accumulation of `GetProjectedMNPayees` (v24:213-225) is described; the two-Evo and height-0 fixtures join `diff_mnlist_order`, which compares the full ordered projection (§3.4, §6.2). The canonical reader is unchanged |
| **R2-m1** (minor) keyed-map declaration uses wrong Rust APIs | **Accepted.** `UniqueMap(imbl::HashMap<UniqueKey, UniqueSlot>)`; the alias supplies `std::collections::hash_map::RandomState` and `DefaultSharedPtr` (§2.1, §2.2) |
| **R2-m2** (minor) raw round-trip assertions promise excluded metadata | **Accepted.** Per-layer equality projections: raw codecs and `apply_diff` in canonical content / Core `IsEqual` with the counter rule stated; records in full equality incl. counter, ids and buckets (§6.2) |
| R2 note: trailer must capture order-only bucket changes; `apply_record` must invalidate the order cache | **Accepted.** §2.9 trailer rule (dirty on every `set`/`remove`), §2.8 `apply_record` doc |

### Round 3 (GPT)

| Finding | Disposition |
|---|---|
| **R3-F1** (major) P1/P2 do not put a continuation candidate in the equality-height run | **Accepted (ruling 1).** The fast path walks `payee_queue.range(PayeeKey { height: list.height, .. }..)`, the unbounded `pay_height ≥ list.height` range, filtered by Core's exact predicates (`lastPaid == height ∧ valid ∧ Evo ∧ consecutive < 4`, `deterministicmns.cpp` v24:171-179); P1 → uniqueness, P2 → membership (when `height > 0`, `pay_height ≥ last_paid` for every MN, `CompareByLastPaid_GetHeight` v24:132-141). The equality-run wording is removed from §2.4 and §3.4; the future-revival (`E E E R E`), height-0/future-registration and read → build fixtures join `diff_mnlist_order`; the Opus m1 disposition is reconciled; the reader is unchanged |
| **R3-F2** (major) a build cannot certify `fast_payee_ok = true` | **Accepted (ruling 2, cheaper route with proof).** The builder certifies nothing; `fast_payee_ok` is computed from the resulting list: P2 = `height > 0` (else O(n)); P1 = count of valid MNs with `lastPaid == height` over the same range walk, complete because `height > 0 ⇒ pay_height ≥ last_paid`, and equal to the count a build would track since it counts the same set on the same list (§3.4, §2.4). The `rebuild_probe` fixture (predecessor A/B `lastPaid 101`, C `lastPaid 1`; actual `RebuildListFromBlock` pays C and resets A/B, `specialtxman.cpp` v23:486-515 / v24:759-790) is in §6.2 with Core's `A A A A A A A A B` |
| **R3-m1** (minor) the blanket CHAMP rule includes the vector-ordered QcTx branch | **Accepted.** Rule restricted to confirmations, decay, streak reset; QcTx preserves member-vector order (`HandleQuorumCommitment` v23:159-174 / v24:283-298), so punished colliding members end in the reverse of their vector subsequence; payee bookkeeping precedes the `newList2` snapshot; fresh snapshot per phase; fixture `quirk_41_qctx_order` (§2.7, §3.3, §6.2) |
| **R3-m2** (minor) the production fallback sat behind `xcheck` | **Accepted.** `scan.rs` (unconditional) holds `payee_scan`/`projected_scan`; `reference.rs` (twin) stays behind `cfg(any(test, feature = "xcheck"))` and reuses it; a default-feature build and `cargo doc` are EV2b-1 gates (§2.1, §3.4, §7) |
| **R3-m3** (minor) `max(id)+1` is not Core's raw-diff counter rule | **Accepted.** §2.8 and §6.2 now state `ApplyDiff`'s retained predecessor counter bumped per addition by `AddMN`'s `max(added id + 1, counter)` (v23:483-485 / v24:513-515) |

### Round 4 (GPT)

| Finding | Disposition |
|---|---|
| **R4-m1** (minor) the unconditional post-build I-P assertion survived R3-F2 | **Accepted (plus the manager's no-panic ruling).** `Provenance { FromGenesis, Decoded }` on `MnList`; I-P asserted only on `FromGenesis` lists (temporal premises proven from the empty list); on `Decoded` lists and their build results the computed P1 is compared with a literal count and never asserted true (`specialtxman.cpp` v23:486-515 / v24:759-790 keep foreign future `lastPaid` values); no invariant check panics or aborts on any decoded/imported-data path, violations are `InvariantViolation` values, hard assertions only in test/xcheck or on proven provenance; the "by construction" wording in Changes-in-rev2 §2 and the R2-F2 disposition marked superseded (§2.4, §3.4, §7) |
| **R4-m2** (minor) the global `height > 0 ⇒ pay_height ≥ lastPaid` claim and the future-registration label overstate the proof | **Accepted.** Replaced by `list.height > 0 ∧ lastPaid == list.height ⇒ pay_height ≥ list.height` (`CompareByLastPaid_GetHeight` v23:157-166 / v24:132-141; the `lastPaid 0, registered −2` MN has pay height −2 and is not counted); the height-0 positive-future-registration fixture is a fast-path case (`E E E R E`, both pins), and a separate negative-registration MN is the P2-fallback fixture (§3.4, §6.2) |
| **R4-n1** (nit) scan reuse contradicts the twin-sharing boundary | **Accepted.** The sharing contract lists `scan.rs`'s literal scans; the independent check of the scans is direct Core differential coverage of fallback inputs (`diff_mnlist_order`), as the round-4 randomized comparison did against the design model; no separate Rust twin scan (§3.4) |
