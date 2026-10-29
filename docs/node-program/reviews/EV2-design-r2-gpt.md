# EV2 rev1 — focused independent design review, round 2

**Verdict: REWORK.** One blocker and one major remain. Rev1 correctly fixes the primitive collision rules, the missing registration counter, the unkeyed property hasher, and the principal differential-layer distinction. It does not yet reproduce the bulk mutation history required by DEC-54, and its newly unrestricted payee/projection contract is still inaccurate.

Reviewed `designs/EV2-dml.md` (rev1, 1,048 lines), its change list and dispositions, both round-1 reports, REVIEW-COMMON.md, and DEC-50/DEC-54. The approved parity target is a **64-bit Core node synced from genesis online without restarting**. This review accepts that target and does not reopen the approved storage choices, eager decay, member-source ownership, or regtest V24 exception.

## Round-1 finding dispositions

| Finding | Round-2 assessment |
|---|---|
| GPT F1, collision history | **Not closed.** `copy_collision_insert/replace/remove` are now described correctly, and explicit primitive/hot-only updates can maintain the proposed buckets. However, the confirmation, decay and streak loops generate a different mutation sequence inside those buckets; see R2-F1. |
| GPT F2, registration counter | **Closed at design level.** `total_registered_after` is authoritative after raw diff application, snapshot bodies already retain the counter, and surviving additions retain their immutable internal IDs. The revised record contains the information previously lost. A Core-library probe confirms restoring the authoritative counter repairs the register-and-spend bytes and next ID. The old unconditional raw round-trip assertion needs correction; see R2-m2. |
| GPT F3, FxHasher | **Closed at design level.** Keyed SipHash over the pre-images removes the demonstrated fixed-Fx collision family. The random layout is semantically unobservable through the specified wrapper, including content equality across independently seeded maps. Correct the Rust API spelling in R2-m1. |
| GPT F4, differential layers | **Partly closed.** The validated-block/raw-helper split, rejecting-layer attribution, reachable intersection, deployment context and oracle prerequisites are now sound. The EVO-15 expectations are correct with otherwise valid, appropriately signed payloads. The accompanying claim of helper parity on every canonical-reader-accepted list remains false; see R2-F2. |
| GPT F5/F6 | **Closed.** The default list is explicitly a hash/bytes-only fixture, and the benchmark is now described as a limited model with the correct effective changed-entry count. |

Opus M1's revised real-data windows cover the previously omitted PoSe and activation periods in the design; implementing and running those gates remains future work.

## Findings

### R2-F1 — blocker: indexed bulk loops still produce the wrong collision history

**Locations:** `designs/EV2-dml.md:342`, `:347`, `:350`, `:588`, `:589`, `:600`.

Rev1 correctly says that every successful `UpdateMN`, including a hot-only update, moves a colliding key to the bucket front. It nevertheless retains the claim that confirmation, decay and streak loops are order-independent, and prescribes these traversal orders:

- confirmations: `unconfirmed`, ordered by `(registered_height, proTxHash)`;
- decay: a snapshot of `decaying`, ordered by proTxHash;
- streak resets: a snapshot of `evo_streak`, ordered by proTxHash.

Core takes a **CHAMP-order snapshot at the start of each respective phase**. Confirmations traverse `prevList` (`specialtxman.cpp` v23:205–218 / v24:330–343). `DecreaseScores` first collects MNs in the current list's CHAMP order and then updates them in that order (`deterministicmns.cpp` v23:334–358 / v24:309–333). Streak reset traverses `newList2`, copied after transactions, spends and payee bookkeeping (`specialtxman.cpp` v23:503–514 / v24:778–789).

For keys 1 < 2 < 3 < 4 in memcmp order with identical `ReadLE64`, insert 1,2,3,4, then make all four eligible for decay. Both vendored immer probes and the actual v24 `CDeterministicMNList::DecreaseScores` probe give:

```text
initial collision order:          4 3 2 1
Core decay update sequence:        4 3 2 1
Core resulting collision order:    1 2 3 4
rev1 indexed update sequence:      1 2 3 4
rev1 resulting collision order:    4 3 2 1
canonical bytes:                   equal
```

Two colliding MNs suffice. Give a separate noncolliding MN the minimum pay height, so the later payment update does not touch the collision class; use confirmed regular MNs so confirmations and streak reset do not change the example. The wrong order survives the completed block. This changes ordered RPC/mnlistdiff output and can change the first operator-key match for the pre-V24 dual-encoding pair described by the design. It is a disagreement with the **online** DEC-54 reference, not one of the approved restart/import inconsistencies.

The new choke point accurately records the Rust operation sequence, but that sequence is not Core's. I-C's member-set check and canonical-byte comparisons do not detect this. Writing the wrong bucket into the record trailer merely preserves the wrong answer after restart.

**Suggested fix:** preserve Core's phase-start relative order for eligible members sharing a full 64-bit hash. Cross-bucket scheduling can remain indexed because those updates commute; within a collision bucket, filter the appropriate phase-start bucket sequence. Use the previous-list snapshot for confirmations, the post-confirmation snapshot for decay, and the post-payment snapshot for streak reset. Remove the order-independent claim at line 342. Compare `core_order()` and operator first-match results after full block builds, not just after caller-specified primitive operations; include hot-only and no-op updates and cache eviction/record reconstruction.

**Primitive/trailer assessment:** the revised prepend, move-to-front and survivor-preserving deletion rules do match `node.hpp:384–405`, `:484–514`, `:433–455` at both tags. `champ.hpp:565–576` dispatches replacements/inserts to those functions; `:1201–1208` collapses a two-member bucket to a singleton. Dropping singleton buckets and recreating them on the next insertion is correct. Complete snapshot trailers and post-block overrides of touched diff buckets are sufficient representations **once the online mutation sequence is fixed**. A touched bucket must be captured even when its state updates cancel and `BuildDiff` has no corresponding item; `apply_record` must invalidate the order cache after installing the trailer.

### R2-F2 — major: the unrestricted payee/projection fix still disagrees with Core

**Locations:** `designs/EV2-dml.md:645`, `:647`, `:885`, `:925`, `:1036`.

Picking the last continuation candidate is correct for `GetMNPayee` on an ordinary positive-height list. It is not a complete description of `GetProjectedMNPayees` on a list violating I-P. Core **appends the remaining payments of every matching evonode during the first scan**. Only `remaining_evo_payments` and `evo_to_be_skipped` are overwritten by the last match. It subsequently skips just that last MN in the weighted scan and sorts from `result.begin() + remaining_evo_payments` (`deterministicmns.cpp` v23:237–271 / v24:212–246).

The actual v24 canonical reader accepts this fixture:

```text
list height = 100; MN_RR off
CHAMP order = A, B; both valid Evo MNs
A: lastPaid = 100, consecutive = 1, registered = 10
B: lastPaid = 100, consecutive = 2, registered = 20
```

The Core-library probe round-trips the fixture through `UnserializeCanonicalMNList` before calling the helpers. The payee is B, but the eight-entry projection is:

```text
Core:                       A A A A A A A B
rev1 last-candidate model:   B B A A A A B B
```

This is not sorting instability: Core has already appended A's three remaining entries before discovering B, and the protected prefix length is then B's remaining count, two. The queue model discards that earlier append. Thus the unrestricted `diff_mnlist_order` gate cannot pass as specified, even though the optimization is correct on normal reachable lists.

There is also a smaller boundary failure in the new range argument. `pay_height >= last_paid_height` is false when `last_paid == 0` and the registered height is negative: Core's `else if` substitutes that registered height. The canonical reader checks list height but does not restrict registration heights. A reader-accepted height-0 list with a regular MN registered at −2 and an Evo MN registered at −1, both valid and last-paid 0, has Core payee = Evo when V19 is on and MN_RR off. Both queue heights are below 0, so rev1's `range(height >= list.height)` finds no continuation candidate and chooses the regular MN instead. The Core-library probe confirms acceptance and the Evo result.

**Suggested fix:** retain the indexed path on lists satisfying its proven assumptions, and use an exact Core scan/projection fallback for other accepted lists. Alternatively, explicitly implement all accumulated first-scan projection entries and the height-0 range exception. Do not add extra canonical decoder restrictions to manufacture agreement. Add both fixtures to the unrestricted helper differential corpus and compare the ordered projection vector, including its length and repeated identities.

These fixtures concern the advertised raw-helper/codec domain; they are not claims about a payee change on a validated historical chain.

### R2-m1 — minor: the keyed-map declaration does not use the pinned Rust APIs

**Locations:** `designs/EV2-dml.md:105`, `:132`, `:136`.

`RandomState` is `std::collections::hash_map::RandomState`, not `std::hash::RandomState`. Also, imbl 7.0.2's `GenericHashMap` requires four type parameters `(K, V, S, P)`; the proposed three-parameter declaration fails with E0107. The pinned crate's `HashMap<K,V>` alias already supplies `RandomState` and `DefaultSharedPtr`.

**Suggested fix:** use `UniqueMap(imbl::HashMap<UniqueKey, UniqueSlot>)`, or provide the correct RandomState path and fourth `imbl::shared_ptr::DefaultSharedPtr` argument. The review's SipHash probe compiled and passed using the latter.

### R2-m2 — minor: the raw codec/diff round-trip assertion still promises metadata it excludes

**Location:** `designs/EV2-dml.md:867`.

`apply_diff(a, a.diff_to(b)) == b` cannot be an unconditional full-list equality after the accepted F2 correction: the register-and-spend fixture still gives raw reconstructed counter 0 versus online counter 1. Similarly, a raw canonical list encodes no bucket history, so `decode(encode(x)) == x` cannot include online collision order. These are exactly why rev1 introduces records and separate Core-compatible codecs.

**Suggested fix:** require full counter/ID/order equality for `apply_record` and complete snapshot-record round trips. For raw Core codecs and `apply_diff`, assert byte fidelity/behavior against the corresponding Core operation and state the equality projection or input restrictions. Retain the full reconstruction tests at line 869.

## Answers to the focused checks

1. **Immer and every mutation:** re-derived against independently extracted vendored headers at both pins. Insert prepends, every replacement (even same-value replacement) moves to front, deletion preserves survivors, and singleton collapse/re-formation works as rev1 describes. The defect is the producer's bulk traversal order, R2-F1. Trailer transport is conceptually sufficient but must cover order-only changes and reset caches; it cannot repair an incorrect online sequence.
2. **Registration reconstruction:** the canonical per-block diff contains the final surviving additions, removals and state changes; each added MN contains its internal ID. Restoring the exact recorded post-block `u32` counter supplies the remaining missing datum. This works even for empty diffs and trailing registrations that disappear. Snapshots already encode the same counter. Record/restart/reorg tests are correctly required, but no EV2 implementation exists yet to execute them end to end.
3. **Key randomness:** semantic results are independent of the seed. The wrapper exposes keyed probes, deterministic mutation success/failure, length and content equality; encoders, ordered APIs and error selection do not traverse its randomized layout. imbl 7.0.2's `test_eq` (`src/hash/map.rs:483`) compares entries using the other map's own hasher, so even fresh seeds after decode compare correctly. Execution timing, allocation patterns and instruction counts can vary; that is not a byte/verdict/order leak and deterministic benchmark fixtures should control their hash state. The probe's two independent seeds hashed all 512 test keys differently while lookup/mutation/equality results agreed. On the old collision fixture, Fx produced 1 hash and 131,328 lookup equality checks; SipHash produced 512 hashes and 512 checks.
4. **Differential verdicts:** with other checks satisfied, unknown-hash/type-7 is EV1 `bad-protx-hash` in v23 and EV1 `bad-protx-type` in v24. Known-hash/type-7 passes v23 EV1 and reaches EV2b's mismatch check; v24 still rejects in EV1. A replacement followed by an otherwise valid update to the removed MN reaches EV2b's `bad-protx-hash` under both. Bare-helper unknown-hash/type-7 is the declared expected v23/v24 difference. I checked both pins' `CheckProUpServTx`, `IsTriviallyValid` and rebuilding branches. The principal layer fix is correct; R2-F2 is the remaining helper-domain error.
5. **New rev1 issues:** R2-F1 exposes the remaining ordering assumption after adding history metadata; R2-F2 invalidates the revised any-accepted-list helper promise. R2-m1/m2 are mechanical specification corrections.

## Validation evidence and limits

Scratch sources, binaries and logs are under `/work/scratch/ev2-gpt2/`:

- `immer_probe.cpp`, `immer-v23.log`, `immer-v24.log`: compiled with `g++ -std=c++20 -O2` against `git archive` extracts of each exact tag. Primitive, same-value replacement, singleton collapse/re-formation and bulk-order assertions passed identically. `git diff v23.1.8 v24.0.0-rc.3 -- src/immer` is empty.
- `core_probe.cpp`, `core-v24.log`: linked to the existing v24 oracle-shim libraries. The relevant DML, special-tx, snapshot-header and immer sources have no changes from the pinned v24 tag. Confirmed actual `DecreaseScores` ordering; canonical-reader acceptance and exact projections of the two-Evo fixture; acceptance/payee of the height-0 fixture; authoritative counter restoration and the next registration ID. Canonical bytes remained equal across the deliberately different decay histories.
- `sip-probe/`, `sip-probe.log`: release-mode offline Cargo probe with imbl 7.0.2, actual SmallVec keys and two fresh RandomState seeds. Verified the old Fx fixture, SipHash dispersion, content equality across seeds/insertion orders, clone isolation, removals and replacements. No EV2 performance claim is inferred from these operation counts.
- v24 `evo_snapshot_tests`: **31 cases / 103,752 assertions passed**.
- v23 field-bit/migration tests: **2 cases / 52 assertions passed**.
- v24 field-bit/migration and canonical diff serialization tests: **3 cases / 53 assertions passed**.

All probes and tests used `TMPDIR=/dev/shm/ev2-gpt2-tmp` with mode 0700; it was removed after testing. No branch under review was modified, no commit/push/GitHub message was made, and no dashd instance was started or accessed. The passing reference tests and scratch probes establish the cited Core behavior, not correctness of a future EV2 implementation.

Verdict: **REWORK**.
