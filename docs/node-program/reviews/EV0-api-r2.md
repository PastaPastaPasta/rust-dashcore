# EV0 API review, round 2 (narrow closure check)

| | |
|---|---|
| Under review | `node/ev0-evo-interfaces` @ `fae615f3` (fixes: `8a7b4f34` code, `fae615f3` `API.md`/`ARCHITECTURE.md`), crate `dash-evo` |
| Round 1 | `reviews/EV0-api-r1.md` @ `d69e40a1`, APPROVE-WITH-CHANGES (M1, M2, M3, m1–m9, n1–n4, Q1–Q14) |
| Reviewer | Opus 5.5 |
| Core | `~/workspace/dash`, `git show v23.1.8:…` / `git show v24.0.0-rc.3:…`; extracts in `/work/scratch/ev0-api-r2/core/` |
| base-sdk | `BlsLazy` at the patched pin `481f8c35` (`pkgs/pkc/src/bls/wrap_lazy.rs`), copy in `/work/scratch/ev0-api-r2/wrap_lazy.rs` |
| Date | 2026-10-09 |

## 0. What I ran

- Built a `git archive` copy of `fae615f3` in `/work/scratch/ev0-api-r2/tree`, with its own target dir and `TMPDIR=/dev/shm/ev0-api-r2-tmp`.
- `cargo test -p dash-evo --all-features`: **29 pass** (15 lib, 8 consumers, 6 store), 0 fail. Plain `cargo test -p dash-evo` gives the same result.
- `cargo clippy -p dash-evo --all-features --all-targets -- -D warnings`: clean. The same with `--no-default-features`: clean.
- A reviewer probe, scratch only (`tree/dash-evo/tests/probe_r2.rs`), with 2 tests that both pass. They demonstrate r2-m1 and r2-m2 below.
- Core checks were done with `git show` at both tags:
  - `llmq/snapshot.cpp` (`BuildQuorumRotationInfo`);
  - `llmq/blockprocessor.cpp` (`GetLastMinedCommitmentsByQuorumIndexUntilBlock`, `…PerQuorumIndexUntilBlock`, `GetMinedCommitment`);
  - `validation.cpp` (`DisconnectBlock`'s notify, VerifyDB, ReplayBlocks);
  - `bls/bls.h` (`CBLSLazyWrapper`);
  - `evo/dmnstate.h` (`ResetOperatorFields`);
  - `evo/specialtxman.cpp`.

## 1. Dispositions

### Majors

| | Disposition | Evidence |
|---|---|---|
| **M1** (mutating `PartialEq`) | **CLOSED** (two minors on `SameRecord`'s definition: r2-m1, r2-m2) | See §2 |
| **M2** (stale-request commitment read) | **CLOSED** | See §3 |
| **M3** (`undo` on list-changed) | **CLOSED** | See §4 |

### Minors and nits from round 1

| | Disposition | Note |
|---|---|---|
| m1 (`Send + Sync`) | **CLOSED** | `llmq/members.rs:22`, via Q10. |
| m2 (attempt overlay) | **DEFERRED, acceptable** | The `lq1a.rs:261` stub still builds `AttemptRows` per call, and `effect.rs:145` still says "may also update an overlay". This is LQ1a/CH7b's decision, not EV2a's. §8 of `API.md` says "EV2a and the owners", which covers it, but see §5. |
| m3 (`bls_after`) | **CLOSED** | Dropped via Q7. The `EvoDelta` doc names the recompute (`delta.rs:19-23`). |
| m4 (`best_block`) | **DEFERRED, acceptable** | It is documentation or a trait split, for CH7a. No pure consumer reads it today. |
| m5 (`iter` rename, dissolved bucket) | **DEFERRED, acceptable** | Do it in EV2a's first commit, before LQ1a or EV4 build on `iter()` (§5). |
| m6 (`MnStateDiff` accessors) | **DEFERRED, acceptable** | Purely additive. |
| m7 (`Counts`) | **CLOSED** | `Counts` holds Core's six counts; `add` is the `GetCounts` loop body. The test `undefined_types_count_as_regular_with_weight_zero` gives (9, 5) for {R, E, 7, banned E}. That matches `deterministicmns.h` v24:269-294 (`GetMnType(7).voting_weight == 0`). |
| m8 (exhaustive classification) | **CLOSED** | `ch7a.rs` has an exhaustive `match` over `EffectKind` with no wildcard, and asserts exactly one presumed kind. |
| m9 (`RangesSet` doc) | **DEFERRED, acceptable** | `creditpool.rs` is unchanged. `API.md` Q11's "Where: `creditpool.rs`" therefore points at a doc that does not say this yet (nit r2-n2). |
| n1–n4 | **DEFERRED, acceptable** | None of them changes a consumer-facing signature, except n1's duplicate fields. Those are EV2a's codec decision anyway. |

### The 14 open questions

| Q | Disposition | Note |
|---|---|---|
| Q1 | **CLOSED** | There is one trait, `MinedCommitmentReader`, and `mined_commitment` takes `at`. The LQ1a stub reads it directly (`lq1a.rs:139-140`). There is no tip-bound adapter in `src/`. M2's primitive is in the same change. The LQ1 §2.5/§3.8.2 erratum is listed for the manager (`API.md` §6, §7). |
| Q2 | **CLOSED** | Docs only (`store/mod.rs` header). |
| Q3 | **CLOSED** | The alias is kept, and the `MinedCommitmentRecord` doc names `SerializedEqual` for LQ2's swap (`delta.rs`). Nit r2-n1: `Effect` also derives `PartialEq` and holds the commitment. |
| Q4 | **CLOSED** (decision recorded) | It takes effect when BS8 lands. |
| Q5 | **CLOSED** (decision recorded) | The newtype stays until base-sdk owns `ProTxHash`. |
| Q6 | **CLOSED** (decision recorded) | This is BS6a's action. |
| Q7 | **CLOSED** | `bls_after` is removed, and so is `MemEvoStore`'s "ignores it" note. |
| Q8 | **CLOSED** | `effect.rs:12-16` calls the classification guidance only, and the stub is exhaustive. |
| Q9 | **CLOSED** (decision recorded) | This is EV1's. |
| Q10 | **CLOSED** | `pub trait QuorumMemberSource: Send + Sync`. |
| Q11 | **CLOSED** (decision recorded) | The doc half is m9, which is deferred. |
| Q12 | **CLOSED** (decision recorded) | This is EV2a's. |
| Q13 | **CLOSED** | `Dmn::mn_type: RawMnType`, with `stored_type()`, `is_evo()` and `weight()` (0 for undefined). The unique-key and count paths use `is_evo()` (`list.rs:195`), and the EV1 stub compares `RawMnType`. |
| Q14 | **CLOSED** | `insert_for_tests` is under `#[cfg(any(test, feature = "test-utils"))]` (`list.rs:186`). |

## 2. M1 in detail: `SameRecord` and `core_eq`

**What changed:**
- `PartialEq` is gone from `MnCold`, `Dmn`, `MnList`, `MnListDiff`, `MnStateDiff`, `MnListDiffRecord`, `DmlDelta` and `EvoDelta`. I checked with a `derive(PartialEq)`/`impl PartialEq` sweep of `src/`. Every remaining `PartialEq` is on a type with no lazy BLS field.
- `MemEvoStore::write_derived` now takes `V: SameRecord`, with `Arc::ptr_eq` short-circuiting through the `Arc<T>` impl.
- `MnCold::core_eq` is Core's member-order comparison, using `BlsLazy::eq`.
- Field coverage is complete: `MnCold::same_record` and `core_eq` cover all 15 public `MnCold` fields, and `Dmn::same_record` covers all 7 `Dmn` fields.

**Does `SameRecord` never decode? Yes.**
- `operator_key_record` (`record.rs:56-57`) calls only `scheme()` and `to_bytes()`.
- In base-sdk, `to_bytes` is `LazyState::serialize(captured scheme)` (`wrap_lazy.rs:194-198, 67-77`). It never calls `BlsValue::from_canonical`. For a default key it zeroes `buf`, which is already non-current. For a key holding an object with no current buffer, it encodes the object and sets `buf_valid`. Otherwise it changes nothing.
- `scheme()` is a plain read.
- The tests `same_record_never_changes_a_later_serialization`, `core_eq_has_cores_side_effect` and `a_rewrite_check_never_changes_the_stored_serialization` (store) all pass. The last one exercises the round-1 failure: a raw undecodable key stored, then a decoded rewrite.

**Is it never weaker than Core where Core compares? Yes, given today's invariant that raw keys are version-sourced.**

`SameRecord` substitutes for exactly one Core comparison, the serialized-equality rewrite check (`WriteDerived`, `evo/evodb.h` v24:159-205). Core serializes the operator key through `CBLSLazyPublicKeyVersionWrapper`, that is `Serialize(s, nVersion == LegacyBLS)` (`bls.h` v24:444-457). `SameRecord` instead compares the captured scheme and the bytes under the captured scheme, with `version` compared separately. The cases:
- **Captured scheme equals the version scheme.** The bytes are Core's bytes, so the two checks are identical.
- **Captured scheme differs from the version scheme, both keys hold objects.** Equal captured-scheme bytes imply equal points, which imply equal version-scheme bytes. `SameRecord` agrees with Core.
- **Captured scheme differs from the version scheme, both keys raw.** Core writes zeros for both, so it says equal whenever `SameRecord` does.
- **Captured scheme differs from the version scheme, one raw and one holding an object.** Here `SameRecord` would be weaker: equal captured-scheme bytes, but Core writes zeros for one side and a re-encoding for the other. This needs a raw key whose captured scheme is not its state's version scheme. Codec-decoded keys are version-sourced (`BlsSchemeSource::Version`), so the case is unreachable today. The r2-m2 fix removes the dependency on that invariant.

Against Core's `operator==`, as opposed to the serialized check, `SameRecord` is weaker in one case: a default key against a key holding an *invalid* object, which both serialize as zeros. Core's `operator==` says unequal (`is_default != r_is_default`, `bls.h` v24:514-534). That is acceptable, because `SameRecord` never stands in for `operator==`. `core_eq`, and v24's `GetHash` comparison in EV2a's `MnStateDiff::between`, do that, and the docs say so (`types.rs`, `record.rs` module doc).

### r2-m1 (minor): `same_record` fills the decoded side's serialization cache, which changes what a *later* `core_eq` does to the other state

**Where:** `dash-evo/src/dml/record.rs:50-57`. The doc claims "no later serialization, hash or `get()` changes".

**How it fails.** The claim is true for the key `to_bytes` is called on, but not for keys later compared against it.

Take A, a key holding a decoded object with no current buffer (Core's `Set`, for example v24 `specialtxman.cpp:71` or a v23 ProUpServ/ProReg state), and B, a raw, undecodable key under the same scheme.
- **Without a prior `same_record`.** `A.core_eq(B)` takes Core's `Get()` path, which decodes B. The decode fails, and B serializes as zeros from then on.
- **After any `same_record` touching A.** A's `buf_valid` is set, so `core_eq` takes the byte path (`wrap_lazy.rs:276-277`) and B stays raw.

The probe `same_record_changes_a_later_core_eq` shows B serializing as `00…` without a prior `same_record` and as `ff…` after one. So an xcheck or test comparator that calls `same_record` mid-flow, before EV2a's v23 `BuildDiff` calls `core_eq` on the same `Arc`'d state, changes the bytes of the old list. That is the class M1 was about.

Production impact is nil today. `MemEvoStore` is a test store. EV2a's real store compares canonical bytes, which serialize anyway, as Core's `WriteDerived` does, and only after the diff is built. But `SameRecord` is advertised for "xcheck comparators" (`record.rs:5`).

**Fix (two characters of logic):** serialize a clone, `key.clone().to_bytes()` (or `to_bytes_as`, see r2-m2). `BlsLazy::clone` copies the state (`wrap_lazy.rs:228-237`), so the original is untouched. The cost is one G1 value copy. Then the doc claim holds as written. Add the probe as a test.

### r2-m2 (minor): `same_record` compares the captured scheme, so it is stricter than Core's serialization for default keys, and `write_derived`'s doc claim that "EV2a's canonical codec gives the same answer" is false

**Where:**
- `dash-evo/src/dml/record.rs:56-57`;
- `dash-evo/src/store/mem.rs:70`.

**How it fails.** A default key (no buffer, no object) serializes as zeros under any scheme. Core's `operator==` treats two defaults as equal regardless of scheme (`bls.h` v24:520-525), and the version wrapper writes zeros. Yet `same_record` returns false when the captured schemes differ. The probe `same_record_is_stricter_than_core_for_default_keys` shows `core_eq` true, both serializations equal, and `same_record` false.

This is reachable on a revoked masternode. `ResetOperatorFields` assigns `CBLSLazyPublicKey()` (`dmnstate.h` v24:127-131, v23:112-115), whose default constructor captures the **global** flag (`bls.h` v24:409-411), while setting `nVersion = LegacyBLS`.
- **The online list.** After V19, a revoked MN's key is default and captured Basic.
- **The same list reconstructed from records.** EV2a's codec is version-sourced, so the key is default and captured Legacy.

EV2 §2.8's "a reconstructed list equals the online list byte for byte" check, written with `same_record`, therefore fails spuriously on every revoked MN after V19. It will equally fail a `MemEvoStore` rewrite of the same block when the flag differs between the two computations (the V23_1 `BlsSchemeLeak`), though Core's serialized comparison accepts it.

**Fix:** compare what Core writes: after `version` equality, `key.clone().to_bytes_as(scheme_of(version))`, which is the `CBLSLazyPublicKeyVersionWrapper` serialization. That is exactly `WriteDerived`'s and the codec's comparison, it also closes the invariant-dependent weaker case in §2, and it subsumes r2-m1. If a stricter test-only relation is wanted, keep it as a separate, clearly named helper. Do not make it the relation `write_derived` and xcheck use.

## 3. M2 in detail: `mined_index` against Core

**Core at both pins.**
- `BuildQuorumRotationInfo` checks each `baseBlockHashes` entry with `ActiveChain().Contains` (v24 `snapshot.cpp:105`, v23 `:71`).
- It looks up `request.blockRequestHash` without that check (v24 `:141`, v23 `:96`).
- It then calls `GetLastMinedCommitmentsPerQuorumIndexUntilBlock(llmqType, blockIndex, 0)` and `GetMinedCommitment(llmqType, obj->GetBlockHash())`, and fails on `ZERO` (v24 `:191-197`, v23 `:149-150`).
- `GetLastMinedCommitmentsByQuorumIndexUntilBlock` is **byte-identical between the pins**; I diffed the function bodies. It:
  - seeks `(prefix, type, quorumIndex, inv(h(pindex)))`;
  - stops at `curKey >= inv(0)`, so height 0 is excluded and the iteration stays inside one quorum index;
  - stops at a mined height above `h(pindex)`;
  - maps the base through `pindex->GetAncestor(quorumHeight)`;
  - returns the `cycle`-th row.

  It iterates `m_evoDb.GetCurTransaction()`, which holds only active-chain rows.

**The Rust code.**
- `mined_index(type, family, chain_end, max_mined_height, limit)` returns the rows in `chain_end`'s ancestry, mined at heights `1..=min(cap, h(chain_end))`, newest first (`store/mod.rs:163-175`, `mem.rs:262-292`).
- The documented qrinfo composition (`store/mod.rs:137-142`) is: `mined_index(type, Rotated{i}, tip, h(S), 1)`, then `base_through(chain, S, base_height)`, then `mined_commitment(type, hash(base), tip)`.

This is Core's composition. Chain end = tip gives Core's active index, the cap is `h(S)`, bases map through `S`, and the commitment read is effectively unbound, because Core's `q_mc` holds only active-chain rows. Clamping the cap to `h(tip)` is harmless: Core's index has no rows above the tip.

A base above the fork maps to a block on S's branch. No active-chain commitment can name it, because a commitment's quorum hash is validated as an ancestor of its mining block, so the read is `None`, which is Core's `return false`.

- **Provided methods.** `mined_until`, `nth_mined_by_index` and `newest_by_index` use `chain_end = at` and the cap `h(at)`, mapped through `at`. They are equivalent to round 1's implementations, and `newest_by_index` is now one walk (LQ1 L6).
- **Test.** `qrinfo_reads_the_active_index_for_a_stale_request_block` covers both outcomes: a base below the fork resolves to the active commitment, and a base above the fork gives `None`. It also covers the ancestry read diverging for a stale request block. I checked the fixture's expected values by hand against Core's loop.

## 4. M3 in detail

- **The change.** `Effect::NotifyMnListChanged { at, undo }` (`effect.rs:65-79`). The doc cites level 3's `DisconnectBlock` (`undo = true`) and level 4's `ConnectBlock` (`undo = false`), and the `CMNAuth`/governance listeners.
- **Core.** I verified the anchors:
  - v24 `validation.cpp:2368-2371` is `if (this == &ActiveChainstate() && diff.HasChanges()) NotifyMasternodeListChanged(true, old_list, diff)`;
  - VerifyDB calls `chainstate.DisconnectBlock` at v24 `:5113` (v23 `:4721`);
  - v23 `:2157-2161` notifies when the optional has a value.
- **The stub.** The CH7a stub routes `undo: true` (`ch7a.rs:112-131`).
- **Errata.** The A1 §2.10.1 erratum is listed for the manager (`API.md` §6).
- **Note for CH7a, not an EV0 change.** `ReplayBlocks` also goes through `DisconnectBlock`, and so notifies (v24 `:5275`, v23 `:4925`). It commits its evodb transaction, so it belongs to the committed-disconnect `ChainEvent` path, not to an `Effect`. CH7a's notification-parity acceptance should list it next to reorg disconnects.

## 5. Deferred minors

The deferrals are reasonable. Round 1 allowed every minor except m1 and m7 to land later, and both of those are closed. Two scheduling notes for the manager:
- **m2 is not EV2a's.** The overlay model (CH7b's facade owning an attempt-tagged overlay, with runtime sinks aborting on a write failure) is read by LQ1a and CH7b. It should be a PLAN gate on LQ1a's start, or on whichever of LQ1a/CH7b starts first. The `lq1a.rs` stub still suggests a per-call overlay.
- **m5's rename churns consumers that already call `iter()`.** That is the LQ1a stub and anything EV4 writes. It should land in EV2a's first commit, before LQ1a or EV4 code exists.

## 6. New findings (round 2)

No blockers and no majors.

- **r2-m1 (minor).** `same_record` mutates the serialization cache of the key it reads, which changes the path, and so the side effect, of a later `core_eq` on another state. Fix: serialize a clone. See §2.
- **r2-m2 (minor).** `same_record` compares the captured scheme. It is stricter than Core's serialization for default keys (revoked MNs after V19), which breaks EV2 §2.8's online-vs-reconstructed equality and makes `mem.rs:70`'s doc claim false. Fix: compare `clone().to_bytes_as(scheme_of(version))`. See §2.
- **r2-n1 (nit).** `Effect` derives `PartialEq` (`effect.rs:30`) and holds `Arc<LlmqCommitment>` (`ReinsertMineableCommitment`). Today the commitment has no lazy field, so this is fine. When LQ2 swaps the alias (Q3), `Effect`'s equality needs the same canonical-byte rule as `MinedCommitmentRecord`. Add one line to that doc, or drop the derive if only tests use it.
- **r2-n2 (nit).** `API.md` §7 Q11 says "Where: `creditpool.rs`", but `creditpool.rs` is unchanged; that doc is m9, deferred. Say "m9, deferred".
- **r2-n3 (nit).** `MnList::same_record` (`list.rs:211-222`) walks both maps in full even when `imbl` shares their nodes. That is fine for tests and the test store. If xcheck runs it per block on mainnet-sized lists, add an `OrdMap::ptr_eq` short-circuit.

## 7. Verdict

**APPROVE-WITH-NITS.**

M1, M2 and M3 are closed, and so are all 14 open-question decisions. M2 matches Core at both pins, and the qrinfo function bodies are identical between v23.1.8 and v24.0.0-rc.3. `SameRecord` never decodes, and it is never weaker than Core's serialized comparison under today's version-sourced invariant. The build, the tests (29) and clippy (both feature sets) are clean, and the deferred minors are reasonable to defer.

r2-m1 and r2-m2 are each a one-line change inside `operator_key_record`, together `key.clone().to_bytes_as(scheme_of(version))`, plus one doc sentence and the two probe tests. They do not change any signature. I recommend folding them in before merge, because `SameRecord` is the relation EV2a's reconstruction checks will be written against. They do not need another review round.
