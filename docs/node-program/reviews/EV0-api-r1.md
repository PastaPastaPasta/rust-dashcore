# EV0 API review, round 1 (consumer owners: EV1, EV2a/EV2b, LQ1a, CH7a)

| | |
|---|---|
| Under review | `node/ev0-evo-interfaces` @ `d69e40a1` (44cb1877, 6746f684, d69e40a1), crate `dash-evo` |
| Reviewer | Opus 5.5, acting as the future implementer of EV1, EV2a/EV2b, LQ1a and CH7a |
| Binding inputs | EV2-dml.md rev4 (DEC-58) + errata; LQ1-quorum-members.md rev4 (DEC-60); CS0 + merged `dash-consensus`; ST3a §2.4.4, §5.6; DEC-71; A1 §2.5, §2.10.1, §2.10.2, Appendix A; PLAN rows EV0, EV1, EV2a, EV2b, LQ1a, CH7a; memory note `dash-mnlist-champ-order` |
| Core | `~/workspace/dash`, `git show v23.1.8:…` / `git show v24.0.0-rc.3:…`; extracts in `/work/scratch/ev0-api-review/` |
| Date | 2026-10-09 |

## 0. What I ran

- I built the branch as a `git archive` copy in `/work/scratch/ev0-api-review/tree` with its own target dir and `TMPDIR=/dev/shm/ev0-api-review-tmp`.
- `cargo test -p dash-evo --all-features`: all 25 tests pass (13 lib, 8 consumers, 4 store).
- `cargo clippy -p dash-evo --all-features --all-targets -D warnings` is clean, and so is `--no-default-features`.
- Reviewer probe, not on the branch: `MnList`, `Arc<MnList>`, `Dmn`, `MnCold`, `MnListDiffRecord`, `Effect`, `QuorumSnapshot` and `MemEvoStore` are all `Send + Sync`. `size_of::<Effect>() == 96` and `size_of::<MnCold>() == 520`.
- I checked the Core claims below with `git show` at both tags. The relevant files are `validation.cpp`, `evo/deterministicmns.cpp`, `evo/specialtxman.cpp`, `llmq/blockprocessor.cpp`, `llmq/quorumsman.cpp`, `llmq/snapshot.cpp`, `evo/smldiff.cpp`, `evo/mnauth.cpp`, `dsnotificationinterface.cpp`, `evo/deterministicmns.h`, `evo/dmn_types.h`, `node/chainstate.cpp` and `util/ranges_set.*`.
- I also read base-sdk's `BlsLazy` (`pkgs/pkc/src/bls/wrap_lazy.rs` @ 481f8c3).

## 1. Per-consumer fitness

### EV1 (special-tx contextual checks)

**Fit: yes, with additions that are not changes.** Everything EV1 reads at `pprev` is either in EV0 or explicitly EV2a's:
- in EV0: `get`, `get_valid`, `has_unique`, `unique_owner`, `get_by_collateral`, `UniqueKey::{collateral, key_id, platform_node_id}`, `Dmn::mn_type` compared through `RawMnType`, `EvoCtx::{bls, check_sigs}`, `MinedCommitmentReader` for `merkleRootQuorums` and quorum lookups, and the hash-keyed credit-pool and MNHF rows;
- EV2a's: `UniqueKey::{service, domain, operator_key}`, `get_by_service`, `get_by_operator_key`, `has_operator_key_under_any_scheme`.

**Not in EV0, and correctly so:**
- `BlockPayloads` (Q9);
- CS5a's `CoinsView`;
- EV3's pool and signal resolvers;
- QM1's quorum facade for asset-unlock and MNHF signatures;
- EV4's coinbase-CL predicate.

**Awkward:**
- Every probe allocates a `Box<[u8]>` `UniqueKey`. This is deviation 4; PF0 decides.
- Equality on cold states can decode BLS keys. See M1.
- The compile-only EV1 stub is thin: two `has_unique` probes. It proves little about the operator-key and BLS paths, which are where EV1's API risk is. That is acceptable for a types-first task, but EV1 should not read it as coverage.

### EV2a (the list model, codecs, store)

**Fit: yes.**
- `MnList`'s private fields leave room for the four indexes, `core_order`, `fast_payee_ok` and the provenance rules of EV2 §2.4–§2.5.
- `DmlReader` is EV2 §2.10 verbatim, and so are `MnListDiffRecord` and `DmlDelta`.
- `MemEvoStore` applies at `EvoDelta` granularity instead of EV2's `apply(&DmlDelta)` + `set_tip`. That is a superset, and fine.

**Must change before EV2a builds on it:**
- the mutating `PartialEq` chain (M1);
- the `Counts` representation, if Q13 goes the recommended way (m7).

**Should be specified now:**
- the bucket-trailer encoding for a bucket that dissolves (m5);
- out-of-crate read access to `MnStateDiff`'s state for RP2a (m6).

### EV2b (`build_new_list_from_block`, `process_block`)

**Fit: yes.**
- The `QuorumMemberSource`/`MemberError` text is LQ1 §2.6 verbatim, and the `Store`→node-error / `UnknownLlmqType`→exception mapping works (stub `ev2b.rs`).
- `Effect::ResetPlatformBan { pro_tx_hash, height }` carries EV2 §3.8's erratum and can be emitted before `bad-dip3-enf-block`.
- EV2 §2.11's signatures can all be written against EV0: `BuiltList`, `ProcessedList`, `MnListChange`, `DmlBuildError`.

The open dependency is Q13 (`RawMnType` in `Dmn`), which changes `try_add`, `Counts` and the ProRegTx/ProUpServTx type checks. Decide it now.

### LQ1a (member selection, scans)

**Fit: mostly.**
- The `MnEntryView`/`MnListView` adapters are implemented in the stub over `Arc<Dmn>`/`MnList`. Memcmp `iter()` is acceptable here, because every LQ1 path sorts by score with a collateral tie-break (LQ1 §2.2.3, §3.4).
- `SnapshotRows` maps onto `EvoStore::quorum_snapshot` + `Effect::StoreQuorumSnapshot`.

**Gaps:**
- `MinedCommitmentReader` vs LQ1's `MinedCommitmentIndex` (Q1), and a Core read it cannot express (M2).
- Where the attempt overlay lives (m2).
- `QuorumSnapshot` vs BS8's type (Q4).
- `UsedSet` needs EV2a to expose `ColdDerived::unique_keys`, which EV2a owns.

### CH7a (evo integration, effect router)

**Fit: yes, after M3.**
- `Effect` is exhaustive, so a router `match` breaks at compile time on a new variant.
- `EffectKind::ALL` gives CH7a its classification table.
- `EvoDelta` is transactional-only, and `MemEvoStore::apply` is all-or-nothing (clone the persistent maps, write, swap).
- The `connect_attempt` stub shows the A1 §2.10.1 order: apply the log, then act on the result, then write the delta only on success and only when not `just_check`.

**Needs:**
- the undo direction on `NotifyMnListChanged` (M3);
- a decision on `bls_after` (m3, Q7);
- `best_block` off the pure read trait or documented (m4);
- an exhaustive classification `match` (m8);
- the overlay and runtime-sink contract (m2).

**ST3a §5.6 fit.** ST3a needs CH7a to classify every effect class as *presumed* or *independent* before B1's barrier (§2.4.4), and to emit `Connected { applied }`. `EffectKind` is sufficient for the former; nothing in EV0 obstructs the latter. Effects do not interact with subscriber locators: `MnListChanged` reaches subscribers only as a `ChainEvent`.

### Layering

No pure-crate violation found:
- no I/O, clocks, threads or globals;
- `MemEvoStore` is in-memory;
- `testkit` is behind a feature;
- BLS interior state is base-sdk's `spin::Mutex`.

The two edges are `EvoStore::best_block` (m4) and blst's threadpool in the dependency graph. The implementer discloses the latter; DEC-71's gate change closes it at the re-pin, and EV0 must merge after that re-pin.

## 2. Findings

No blockers.

### M1 (major): `PartialEq` on DML values can decode BLS keys and change their serialization, and it is the default equality

**Where:**
- `dash-evo/src/dml/types.rs:206` (`MnCold: PartialEq` via `BlsLazyPk`);
- `types.rs:316-328` (`Dmn`);
- `dml/list.rs:232-248` (`MnList`);
- `dml/diff.rs:162, 184, 196, 212` (diff, state diff, record, `DmlDelta`);
- `store/delta.rs:209` (`EvoDelta`);
- `store/mem.rs:63-77`, `:149-153` (`write_derived` and the mined-row check use it).

**How it fails.** base-sdk's `impl PartialEq for BlsLazy` (`wrap_lazy.rs:258-283`) is Core's `operator==`. When one side holds raw bytes and the other a decoded key, or the schemes differ, it calls `get()` on both. A failed decode clears `buf_valid`, after which the key serializes and hashes as zeros.

`MnCold` sits behind an `Arc` shared by every list that holds the state. A `==` anywhere therefore rewrites what the tip list's SML entry, mnlistdiff and canonical snapshot serialize. The places that can trigger it are:
- `MemEvoStore`'s rewrite check;
- any `assert_eq!` on lists or records in EV2a/L2/xcheck code;
- `MnList::eq`;
- any consumer that reaches for `==`.

Core performs these decodes only where Core compares, for example v23 `BuildDiff` and `CDeterministicMNStateDiff`. Rust adding comparisons Core does not make gives different bytes on any list holding an undecodable key: decoded and imported lists, fuzz and fixtures, which is DEC-58's no-panic domain. Even when every key decodes, a cross-state `==` costs one G1 decompression per side.

The implementer discloses this (API.md §5 item 9) and defers the fix to EV2a. But the API under review *is* the `PartialEq` impls, and EV2a/LQ1/CH7a tests will be written against them.

**Fix (now, before consumers):**
- Drop `PartialEq` from `MnCold` and from everything that contains one: `Dmn`, `MnList`, `MnListDiff`, `MnStateDiff`, `MnListDiffRecord`, `DmlDelta`, `EvoDelta`.
- Add `MnCold::core_eq(&self, &other)`, documented as Core's `operator==` with its side effects, for the places Core compares.
- Add a side-effect-free `same_record` for tests and the store. It compares the lazy key's current state (buffer, validity, scheme, decoded value) without decoding; ask base-sdk for a read-only accessor, or compare `to_bytes()` only when `buf_valid` holds on both sides. Once EV2a's codec lands, it compares canonical bytes.
- `MemEvoStore::write_derived` uses `Arc::ptr_eq` || `same_record`.
- Apply the same rule later to `LlmqCommitment` when it becomes the eager-BLS type (Q3). Core's `q_mc` rewrite is `SerializedEqual` (`blockprocessor.cpp` v24:381-382).

### M2 (major): the ancestry-resolved commitment reader cannot express Core's read for a block that is not on the active chain, and Core does that read on a P2P path

**Where:** `dash-evo/src/store/mod.rs:101-165` (`MinedCommitmentReader`), and the premise in its doc (`:105-107`) and in LQ1 §3.8.2: "every Core call site passes an active-chain block or the block being connected".

**Counterexample.** `BuildQuorumRotationInfo` checks the *base* block hashes against the active chain (v24 `llmq/snapshot.cpp:105`, v23 `:71`), but **not** `request.blockRequestHash` (v24 `:141-146`, v23 `:96`). It then calls `GetLastMinedCommitmentsPerQuorumIndexUntilBlock(type, blockIndex, 0)`. That reads Core's `q_mcih` index, which holds only active-chain records, under a height cap of `h(blockIndex)`. It maps base heights through `blockIndex->GetAncestor`, and then reads `GetMinedCommitment(type, hash)` with no chain context (v24 `:191-197`).

Concrete scenario. A peer sends `getqrinfo` naming a stale block S two blocks above the fork F, inside a rotation mining window.
- **Core** returns the active chain's commitments mined in `(F, h(S)]`, whose bases lie below F.
- **The EV0 reader at `at = S`** returns S's branch instead: a different commitment, or none, or the previous cycle's.

The two `lastCommitmentPerIndex` answers differ, so the `qrinfo` bytes differ (P2P, and `quorum rotationinfo` over RPC). When a mapped base lies above F, Core's unbound read returns `ZERO` and the request fails. Rust would answer it.

The reader returns `BlockId` bases only, without mined heights, so the caller cannot recompose Core's read. `mnlistdiff` is not affected: `BuildSimplifiedMNListDiff` requires both blocks on the active chain (v24 `smldiff.cpp:177`).

**Fix:**
- Expose the index-level primitive Core iterates: for a `chain_end`, a `max_mined_height` and an optional quorum index, return `(mined_height, base_height)` newest first. Make `mined_until`/`nth_mined_by_index`/`newest_by_index` provided methods over it, with `chain_end = at`, a cap of `h(at)` and base mapping through `at`.
- EV4's qrinfo builder then calls it with `chain_end = tip`, a cap of `h(S)`, and `GetAncestor` from S, and `mined_commitment(.., at = tip)`. That is Core's composition exactly.
- Add the same errata to LQ1 §3.8.2 and A1 §2.10.2: ancestry resolution equals Core only for active-chain `at`; qrinfo's request block is the known exception.

### M3 (major): `NotifyMnListChanged` has no undo direction, and VerifyDB level 3 is missing from the inventory

**Where:** `dash-evo/src/effect.rs:64-69`, inherited from A1 §2.10.1, which lists only VerifyDB level 4.

**What Core does.** VerifyDB level 3 calls `chainstate.DisconnectBlock` (v24 `validation.cpp:5113`; v23 VerifyDB likewise). That notifies `NotifyMasternodeListChanged(true, old_list, diff)` (v24 `:2368-2370`, gated on the active chainstate and `HasChanges`; v23 `:2157-2160`, which `UndoBlock` fills only on change). The listeners consume the direction and the diff (`dsnotificationinterface.cpp:90-96`):
- `CMNAuth::NotifyMasternodeListChanged` disconnects MN-authenticated peers whose MN the diff removes or whose operator key it changes (`evo/mnauth.cpp:179-210`);
- `CGovernanceManager::CheckAndRemove` runs.

The evodb writes roll back with VerifyDB's transaction (v24 `:5047`), but these actions do not. They are reachable through `verifychain` at runtime (peer disconnects, visible in `getpeerinfo`) and at every startup (`-checklevel` default 3).

**Fix:**
- `NotifyMnListChanged { at: BlockHash, undo: bool }`. Optionally carry the old list and the diff, so the router need not re-resolve them; the rows are never erased, so `at` and `undo` are enough.
- A1 §2.10.1 erratum: add the row "VerifyDB level 3 (undo) and level 4 (connect)".
- CH7a's effect-parity acceptance must include the level-3 case.

### m1 (minor): `QuorumMemberSource` has no `Send + Sync` bound (Q10)

**Where:** `dash-evo/src/llmq/members.rs:21`.

CH7b's facade is shared by validation, QM1, DK1, LQ5 and RPC threads. `&dyn QuorumMemberSource` then cannot cross threads without every signature spelling `+ Sync`. `DmlReader` and `MinedCommitmentReader` are already `Sync`. Adding the bound later changes every consumer signature.

**Fix:** `pub trait QuorumMemberSource: Send + Sync`.

### m2 (minor): the attempt-wide snapshot overlay cannot be reached through `&mut dyn EffectSink`

**Where:**
- `dash-evo/src/effect.rs:132-140` (the doc says sinks "may also update an overlay that later reads in the same attempt see");
- `tests/consumers/lq1a.rs:240-271` (`AttemptRows` built per call, holding the sink).

LQ1 §3.7.2 and §4.2 item 4 require reads inside an attempt to see the attempt's own writes, across every `members()` call in that attempt. `members(&self, …, effects)` reaches the overlay only through `self` (process-wide) or `effects`, which is write-only. An `AttemptRows` built per call loses earlier writes from the same attempt.

Separately, outside connect attempts LQ1 §3.7.2 wants a synchronous single-row write. `emit` is infallible, so a write-through runtime sink can only abort on an I/O error. That is acceptable under A1 I5, but it is unstated.

**Fix:** pick one model and document it in `effect.rs`/`members.rs`:
- **(a)** CH7b's facade owns the attempt overlay. It is attempt-tagged and cleared when CH7a applies the log. The sink stays write-only.
- **(b)** `EffectSink` gains a defaulted read hook for pending snapshot rows.

I recommend (a). Also state that runtime sinks abort on a write failure. Fix the stub so it does not suggest a per-call overlay.

### m3 (minor): `EvoDelta::bls_after` duplicates derivable state (Q7)

**Where:** `dash-evo/src/store/delta.rs:225-230`.

At both pins the flag after a successful connect, a successful undo or startup is `!V19ActiveAfter(tip)`:
- connect: v24 `specialtxman.cpp:983-988`, v23 `:695-699`;
- undo: v24 `:1008`, v23 `:716`, `ActiveAt(pindex)` = `ActiveAfter(pprev)`;
- startup: v24 `node/chainstate.cpp:429-434`, v23 `:274-293`.

That is `bls_ctx_after(deployments, Some(tip))`. Core never persists it. The only stateful part is the V23_1 leak, which is already an effect. Nothing checks the field (`MemEvoStore` documents that it ignores it), so it is a second source of truth.

**Fix:** drop the field. CH7a recomputes the context after a commit and applies `BlsSchemeLeak` on top. If you keep it, `debug_assert` that it equals the derived value.

### m4 (minor): `EvoStore::best_block` is a "latest" read in the pure trait

**Where:** `dash-evo/src/store/mod.rs:174`.

A1 §2.10.2 forbids "latest" reads in consensus APIs. `meta.evo_tip` is CH7a's recovery state, and no pure consumer needs it.

**Fix:** move it to CH7a's store type, or a separate `EvoTip` trait. Failing that, document it as recovery/startup-only and never read by pure code.

### m5 (minor): CHAMP-order footgun, and an underspecified bucket trailer

**Where:** `dash-evo/src/dml/list.rs:189-198`, `dml/diff.rs:205-207`.

The type carries everything `core_order()` needs: `collision_buckets` and the record trailer. EV2a can add it without an API change. But the only public iteration today is memcmp `iter()`. A port of `ForEachMN` (`protx list`, `masternodelist`, `BuildSimplifiedDiff`, `GetMNByOperatorKey`) will naturally reach for `iter()`, and the doc warning is the only guard.

Separately, `collision_buckets_after` does not say how a bucket that dissolves (drops below two members) is recorded.

**Fix:**
- Rename to `iter_memcmp()`/`iter_valid_memcmp()`, or `iter_canonical()`, so `iter()` is unavailable until `core_order()` lands.
- Document that a dissolved bucket is omitted (or is an empty `Vec`), and that `apply_record` derives dissolution from the key set.

### m6 (minor): `MnStateDiff`'s state cannot be read outside the crate

**Where:** `dash-evo/src/dml/diff.rs:185-191`.

RP2a's `protx diff`/`listdiff` prints the changed fields' values, and it lives outside `dash-evo`.

**Fix:** add `pub fn hot(&self) -> &MnHot` and `pub fn cold(&self) -> &MnCold`, or a field-wise view.

### m7 (minor): `Counts` cannot represent Core's counts once `Dmn` holds a raw type (Q13)

**Where:** `dash-evo/src/dml/types.rs:302`, `:340-374`.

Core's `GetCounts` (`deterministicmns.h` v24:269-294) counts any non-Evo `nType` as regular, with `GetMnType(t).voting_weight` (0 for `Invalid`; `dmn_types.h` v24:41-62). `total_weighted() = total_mn + 4·total_evo` is then wrong for an invalid-type MN.

**Fix:** if Q13 adopts `RawMnType`, store `total_weighted`/`valid_weighted` as maintained fields (or count invalid types separately). Change it together with `mn_type`.

### m8 (minor): the CH7a stub classifies durability with `==`

**Where:** `dash-evo/tests/consumers/ch7a.rs:164-166`.

`kind == StoreQuorumSnapshot` makes every future kind *independent* by default. ST3a §2.4.4 requires CH7a to classify each class deliberately.

**Fix (CH7a, and the stub):** use an exhaustive `match` with no wildcard. See Q8.

### m9 (minor): `RangesSet`'s doc bakes in v24 semantics

**Where:** `dash-evo/src/creditpool.rs:181-186`.

"`end == 0` is a range that wraps to 2^64 (Core's `WrappedSuccessor`)" is v24 behaviour. At v23, `Contains` and `Size` differ for that range (O17 CONS-10), and v24's bounded decode rejects non-canonical sets.

**Fix:** say that the representation is pin-neutral and that the semantics are EV3's, per profile (CONS-10).

### Nits

- **n1.**
  - Heights mix `u32` (`EvoDelta::height`, `MinedCommitmentRecord::base_height`) and `i32` (`DmlDelta::height`, `MnListDiffRecord::height`, `ResetPlatformBan::height`).
  - `DmlDelta::{block, height}` duplicate `EvoDelta`'s, which creates the "DML delta for another block" error path (`store/mem.rs:118-123`).
  - `MinedCommitmentRecord::{llmq_type, quorum_hash}` duplicate the commitment's own fields and are not cross-checked.

  Drop or check the duplicates.
- **n2.** `dash-evo/src/bls.rs:235-243`: the test `DeploymentView` treats `BlockId` as a height. `BlockId` is an arena id (cf. `MemChain`). Build the view over `MemChain` instead.
- **n3.** `dash-evo/src/dml/list.rs:88-92`: `scratch()` is `FromGenesis`. LQ1b fills it with entries taken from lists of any provenance. It is never payee-checked, but `Decoded` is the honest label.
- **n4.** `dash-evo/src/store/mem.rs:107-112, 172-182`: `apply`/`disconnect` do not check parent linkage. This is documented for a test store, but the L2 harnesses use it too. An optional `ChainAncestry` check would catch harness bugs.

## 3. Answers to the review questions

**1. Can each consumer be written without changing the API?** Yes for all five, once M1 (equality), M3 (`undo`), m1 (`Send + Sync`) and the Q13 type decision are done. Everything else consumers need is an *addition* their owners already hold: EV2a's list operations, codecs and `resolve_list`; EV3's resolvers; BS6c/EV1's `BlockPayloads`; BS8's snapshot type. Details are in §1.

**2. CHAMP order.** EV0 does not expose it yet, and per EV2 §2.7 it need not: `core_order()` is EV2a's. The list carries the collision buckets and the records carry the trailer, so EV2a can reproduce Core's from-genesis 64-bit order, bucket sequences included, behind the existing API.
- The memcmp `iter()` is correct for the canonical snapshot and the SML root, and harmless for LQ1, whose selection is sorted.
- It is wrong for mnlistdiff/qrinfo bytes (`BuildSimplifiedDiff`), `protx list`, `masternodelist`, `GetMNByOperatorKey`'s first match, and the evonode last-match payee scan on imported lists.
- Rename it (m5) so those ports cannot pick it.
- `MnListDiff.added`/`removed` are internal-id ordered, as Core's `BuildDiff` is. EV4 must reorder by `core_order()` of the `to` and `from` lists respectively.

**3. Fork correctness and atomic apply.**
- **Hash-keyed rows** (DML diff and snapshot, credit pool, MNHF, `llmq_S`) are ancestry-correct by construction and never erased, which matches Core for `llmq_S` and is DEC-11's choice for the rest.
- **Mined-commitment reads.** `MemEvoView` filters every row by `chain.ancestor(at, mined_height).hash == mined_block` and maps bases through `at`. For an active-chain `at`, or the block being connected, this equals Core:
  - v23 `UndoBlock` erases `q_mc` and the index rows (`blockprocessor.cpp` v23:398-405);
  - v24 erases them unless another chainstate references them (v24:533-546), which only assumeutxo can cause (NG-6);
  - so Core's index is exactly the active chain's.

  The tests cover forks, both index families and the height-0 bound. **It is not Core for a non-active `at`, and Core has one such call site on a P2P path (M2).**
- **Atomic apply** (`MemEvoStore::apply`) clones the `imbl` maps (O(1)), writes into the clone (copy-on-write, so the `Vec` rows under `entry().or_default()` are cloned and not shared), and swaps only on success. That is sound, and `rewrites_must_equal_the_stored_row_and_fail_atomically` covers it.
- The rewrite rule (v24 `WriteDerived` under both profiles) is a sound fail-stop choice (deviation 10, verified: v23 `deterministicmns.cpp:689-691` and `blockprocessor.cpp:362` use a plain `Write`). The comparison it uses is wrong today (M1).

**4. `Effect`/`EffectSink` vs CH7a and ST3a.**
- The shape fits: exhaustive enum, an infallible order-preserving sink, emission never inside `Result`, and `EffectKind` for ST3a's presumed/independent declaration.
- The inventory needs M3. The overlay and runtime-write contract needs m2.
- `StoreQuorumSnapshot` is ST3a's presumed class. ST3a's subscriber/locator contract is untouched, because effects never reach subscribers except through `ChainEvent::MnListChanged`.
- `ReinsertMineableCommitment` holding an `Arc` (deviation 7) is fine.

**5. BLS plumbing under DEC-71.**
- **Consistent with DEC-71:**
  - no decode API in EV0 takes a `BlsCtx`;
  - the stored operator key is a version-sourced `BlsLazyPk` (legacy iff state `nVersion == 1`, Core's `CBLSLazyPublicKeyVersionWrapper`);
  - `EvoCtx::bls()` hands block processing its `for_block` context for use-time resolution (quirk 21's global-scheme ProUpServ/ProUpRev verification, CbTx and AU signatures);
  - `bls_ctx_after` matches Core's switch, undo and startup sites at both pins (m3's anchors).
- **Two problems:** lazy-key equality (M1), and the redundant `bls_after` (m3).
- **Re-pin dependency:** base-sdk at 481f8c3 still has `BlsSchemeSource::Global` for decode. EV0 does not use it; DEC-71 removes it at the BS6a re-pin, which EV0 must merge after.

**7. The MnListChanged claim.** It is **confirmed**, with one nuance.
- **v24.0.0-rc.3.** `CDeterministicMNManager::ProcessBlock` fills `updatesRet` *unconditionally* (`evo/deterministicmns.cpp:759`). But `ConnectBlock` notifies only under `if (this == &m_chainman.ActiveChainstate() && mnlist_updates.diff.HasChanges())` (`validation.cpp:2918-2920`). The disconnect path is gated the same way (`:2368-2370`, filled only on change at `deterministicmns.cpp:803-808`).
- **v23.1.8.** It fills the optional only when `diff.HasChanges()` (`deterministicmns.cpp:705-706`; undo `:755-760`) and notifies when it has a value (`validation.cpp:2709-2713`, `:2157-2160`).
- The extra `ActiveChainstate()` term matters only with an assumeutxo background chainstate (NG-6), as does the other unconditional consumer, `RecordBackgroundMNListHash` (`validation.cpp:2677`).

So EV2 §3.8 row 6 and DEC-50(12) rest on a misreading: no profile row is needed. EV2b may build `MnListChange` always; CH7a raises the event only when the diff has changes, under both profiles. **Erratum for EV2 §3.8 (row 6 and the "`MnListChanged` is not an effect" bullet) and DEC-50(12).** The same Core lines also show that VerifyDB level 3 notifies (M3).

## 4. Recommendations on the implementer's open questions

| Q | Recommendation |
|---|---|
| **Q1** | **One trait, EV0's `MinedCommitmentReader`, with `at` on `mined_commitment`.** LQ1's `MinedCommitmentIndex` and its `type Commitment` generic are retired. LQ1a can build real `dash_primitives::Commitment`s in tests (the store tests already do), and needs only `quorum_index`; `is_null` is moot because null commitments are never stored. A tip-bound adapter re-creates the "latest" read A1 §2.10.2 forbids, so keep it test-only. Add M2's index-level primitive in the same change. Erratum to LQ1 §2.5 and §3.8.2. |
| **Q2** | **Accept**: hash-keyed rows in `EvoStore`, owner resolvers for "after `at`". `mn_list(at)` etc. in the store would pull `DeploymentView`, `ChainParams`, `ListCache` and DIP3/V20 gates into the store trait. A hash names one chain position, which satisfies A1 §2.10.2. LQ1's `LlmqSource::mn_list(at)` is CH7b's binding of `resolve_list`. |
| **Q3** | **Keep the one-line alias.** Making `EvoStore`, `Effect` and `MinedCommitmentRecord` generic would infect CH7a's router and every store signature for no consumer benefit. When LQ2 swaps the alias, rewrite and equality checks must compare canonical bytes (`SerializedEqual`, v24 `blockprocessor.cpp:381`), not point equality (M1). |
| **Q4** | **Re-export BS8's type when it lands, if** BS8 keeps the mode as a raw `i32` (or an enum with `Unknown(i32)`) and the bitset round-trips through Core's `Write/ReadFixedBitSet`. Core decodes any `int` (`is_serializable_enum`); rows and qrinfo must re-serialize as read. If BS8 types the mode strictly, EV0 keeps its own type plus a conversion. |
| **Q5** | **Have base-sdk own `ProTxHash` in `dash-primitives`**, a non-pure crate where `make_hash!` is fine. Pure crates import it; `immer_hash` stays in `dash-evo` as a free function or extension trait. Condition: its `Ord` must be memcmp of the internal bytes; keep EV0's unit test against it. Don't weaken the purity ban for macro expansions. Until then, keep the newtype. |
| **Q6** | **Yes**: BS6a re-exports a script buffer type from `dash-primitives` and confirms `PayoutShare`/`CollateralShare` at the combined re-pin. `ScriptBytes = Vec<u8>` is a fine placeholder. |
| **Q7** | **Derive it; drop `bls_after`** (m3). CH7a holds the tip context: `bls_ctx_after(tip)`, plus `BlsSchemeLeak` under V23_1. |
| **Q8** | **CH7a keeps the table**, as an exhaustive `match` over `EffectKind` (m8). Durability is runtime policy (`sync_wal`, B1 scope) that a pure crate should not encode. EV0's docs name the A1/ST3a classification as guidance only. |
| **Q9** | **EV1 owns `BlockPayloads`**, in `dash-evo::specialtx`, with its shape fixed once BS6c's single decode-context API lands (DEC-71). EV1 runs first and defines the per-tx decode-failure semantics (`bad-protx-payload`/`bad-qc-payload`); CH7a builds it once; EV2b consumes it. It does not block EV0. |
| **Q10** | **Add `Send + Sync` now** (m1). |
| **Q11** | **EV3's call; per-block values are fine** under DEC-11. Store what `EvoDelta` says Core keeps, and let EV3's resolvers do Core's recompute and fall-back. The `RangesSet` representation (a `BTreeSet<(u64, u64)>` in `Range::operator<` order) matches Core's `std::set<Range>`. Its semantics differ per pin (CONS-10, m9), and v24's bounded decode rejects non-canonical sets, which EV3's codec must reproduce per profile. |
| **Q12** | **Confirmed: `ListCache`/`CacheHint` are EV2a's**, including LQ1's `CacheHint::WorkBlock`. |
| **Q13** | **Option 2: `Dmn` stores `RawMnType`**, with `is_evo()`, `weight()` (0 for unknown) and `stored_type() -> Option<MnType>` helpers. Core's reader accepts any `u16` (`deterministicmns.h` v24:69-84), and Rule DDE keeps the decode domain. Change `Counts` in the same commit (m7). Do it before any consumer is written. |
| **Q14** | **Keep `insert_for_tests` behind `test-utils` until `try_add` lands, then delete it.** Move the testkit list builders onto `try_add`, so no test runs on a list Core's `AddMN` would reject. |

## 5. Verdict

**APPROVE-WITH-CHANGES.**

The crate is the right types-first skeleton. It is pure, it builds and tests clean, it follows EV2 §2 and LQ1 §2.6 closely, its ancestry-resolved store is fork-correct for every active-chain read, and its apply is atomic. Before EV2a, LQ1a and CH7a start writing against it:
- fix M1 (no mutating `PartialEq`);
- fix M2 (expose Core's index-level commitment read, plus the LQ1 §3.8.2 erratum);
- fix M3 (`undo` on `NotifyMnListChanged`, plus the A1 §2.10.1 erratum);
- take m1 (`Send + Sync`) and the Q13/m7 type decision.

The other minors and nits can land with EV2a. Errata to file:
- EV2 §3.8 row 6 and DEC-50(12): no profile row; v24 notifies only on change;
- LQ1 §2.5 and §3.8.2: one reader trait, and the qrinfo exception;
- A1 §2.10.1: VerifyDB level 3 notifies.
