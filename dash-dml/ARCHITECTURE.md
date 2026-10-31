# dash-dml

The node's deterministic masternode list (DML), its records and its evo store (SB0 layer Pn;
EV addendum §1.1), built on the shared `dash-evo`. The SPV client never depends on it. The designs
are `docs/node-program/designs/EV2-dml.md`, `LQ1-quorum-members.md` and `EV-addendum-SB0.md`;
`API.md` maps this crate's items, and `dash-evo/API.md` holds EV0's full requirement map and its
decisions. `dash-evo/ARCHITECTURE.md`'s rules (purity, effects, the error contract, BLS) hold here
too; this file lists what is the node's own.

## Purity and determinism

An ordinary `std` crate in the `node-pure` group, like `dash-evo` (A1 §1.5, §2.1).

- Consensus code uses `BTreeMap`/`BTreeSet`, `imbl::OrdMap`/`OrdSet` or a sorted `Vec`. The one
  hash table is `UniqueMap`, an `imbl::HashMap` with a per-process random SipHash key behind a
  newtype that has no iteration API, so the seed is unobservable (EV2 §2.2).
- **`imbl = "=7.0.2"`** is an exact pin (DEC-50(6)): a bump is a CON change that re-runs the
  model fuzz and the L1 targets.
- **No panic on decoded data** (DEC-58). Hard assertions only in test/xcheck builds, or on lists
  whose `Provenance` is `FromGenesis`.

## Core's orders

- `ProTxHash` (shared, `dash_evo::types`) orders as Core's `uint256::operator<`.
- `MnList::iter()` is memcmp order (the canonical encoding and the SML root). Core's own
  iteration (`ForEachMN`: `protx list`, `masternodelist`, `BuildSimplifiedDiff`,
  `GetMNByOperatorKey`) is immer's CHAMP order over `ReadLE64(proTxHash)` with collision buckets
  newest first (quirk 41), which EV2a's `core_order()` reproduces. Never use `iter()` where Core
  iterates.

## No `PartialEq` on DML values

`MnCold`, `Dmn`, `MnList`, the diffs, the records and `EvoDelta` have no `PartialEq`. The stored
operator key's `operator==` can decode it and leave an undecodable key serializing as zeros, and
a state is shared by every list that holds it. Compare records with `SameRecord` (as Core
serializes them, with keys also equal under Core's `operator==`, both on clones, so nothing
compared is touched); use `MnCold::core_eq` (Core's comparison, side effects included) only where
Core compares.

## The store

- Records are keyed by block hash (per-block DML rows, credit pool, MNHF) or by cycle-base hash
  (rotation snapshots) and are **never erased**; disconnects move the evo tip only.
- Reads are never "latest" (A1 §2.10.2). Mined commitments are read through `dash-evo`'s
  `MinedCommitmentReader` at two levels: the index rows of the chain ending at a block under a
  height cap (`mined_index`, what Core iterates), and the ancestry-resolved reads over them
  (`mined_until`, `nth_mined_by_index`, `newest_by_index`). The ancestry reads equal Core's for
  an active-chain block; the qrinfo builder's stale request block is the exception and uses
  `mined_index` with the tip as chain end. "The value after block `at`" for lists, pools and
  signals is resolved by the owner's function over the hash-keyed rows (EV2a's `resolve_list`,
  EV3's resolvers).
- A rewrite of an existing derived row must be the same record (Core's `WriteDerived`), compared
  with `SameRecord` so the check cannot change the stored row. Rotation snapshots are
  overwritten unconditionally (Core's `StoreSnapshotForBlock`).

## Store faults are node errors (LQ1 D4)

`StoreError` is a **fail-stop node error**, never a block verdict and never
`failed-procspectxsinblock` (manager ruling; LQ1 D4). Its reader is `EvoStore`, a
`MinedCommitmentReader<Error = StoreError>`, and a broken ancestry invariant is
`StoreError::Inconsistent`. Node callers of the shared member source
(`&DmlMemberSource<'_>`, which may borrow its attempt) act on `NodeMemberError`:

- `MemberError::Missing(at)` is `Store(Inconsistent)`: the node holds every connected block's
  list and coinbase, so a missing one is corruption (EV addendum §2.3);
- `MemberError::View(e)` is `Store(e)`;
- `MemberError::UnknownLlmqType(t)` is `UnknownLlmqType(t)`, Core's exception.

## The attempt overlay (EV addendum §2.6, DEC-111)

Core's `StoreSnapshotForBlock` is an unconditional raw write: it survives a failed block,
`fJustCheck`, a GBT proposal and VerifyDB, and the attempt's later reads see it. Here:

- CH7b's `AttemptHandle` owns **one overlay per attempt**, shared by every members and
  precompute call in it. Each call builds `AttemptRows { overlay, backing, sink }` as its
  `SnapshotRows`.
- `store` overwrites the overlay row (last write wins) and pushes
  `Effect::StoreQuorumSnapshot` into the attempt's one ordered log; `get` reads the overlay,
  then the backing rows.
- CH7a applies the log in order on every outcome before the next attempt's read view opens, and
  a write failure is D4. The members caches are the facade's, not the handle's.

`tests/consumers/lq1a.rs` holds the shape (a test `AttemptHandle`) and §2.6 cases 1 and 2, with
the per-call overlay as the negative control; LQ1m's model and CH7b's handle carry the full
suite.

## The codecs

- The canonical list and diff encodings are v24's (`evo/snapshot.h`). The readers apply Core's
  checks in Core's order and return Core's texts: the per-entry CompactSize budget, the
  re-encoding check, payout and prefix-run bounds, strict order, and the counter. A list's
  `CanonicalMNListHash` is SHA256d of its canonical bytes.
- Core's `netInfo->Validate()` depends on the network. The codec takes a `NetInfoValidity` from
  the caller (EV1 owns the rules) and never decides it.
- The store's records wrap the canonical bodies in a versioned envelope. A diff record adds what
  Core's diff cannot restore: the counter and the collision-bucket order.
- `core_disk` reads Core's own evodb lists and diffs for import. Reloading a list adds its MNs in
  file order, and that reverses the CHAMP collision buckets as Core's reload does (quirk 41).

## The unique map's hasher (EV2 §5.5, DEC-50(3), DEC-182)

`UniqueMap` is an `imbl::HashMap` with std's `RandomState`. The design chose it over a fixed fast
hasher because the keys are attacker-chosen (a service's address and port, a domain) and the
reviewer's fixture (EV2 rev1, GPT F3) makes 512 IPv6 services share one rustc-hash hash. This is
the one-time comparison EV2 §5.5 asks for. It is recorded, not gated: it is in no
`workloads.toml` line, and its bench is `benches/unique_map_compare.rs` (no criterion).

- **Date and revision:** 2026-10-10; `08ae81c00` (the bench) over `dash-dml` as of `259e35d57`;
  rustc 1.98.1, rustc-hash 2.1.3, imbl 7.0.2. The tree's uncommitted changes were to the codec
  files only, not to the unique map or the key bytes.
- **Method:** the bench re-runs itself under `perf stat -e instructions:u` (user-space
  instructions, so load does not move them) once with no operations and once with 10⁵, and
  divides the difference by 10⁵; the setup cancels. Five such pairs per cell; a cell is the
  median with the range, which varies only through `RandomState`'s random key. A probe is `get`
  (half hits, half misses). An add and a remove clone the map first, as a block's list clone
  does, then `get` and `insert`/`remove` as `UniqueMap::add`/`delete` do; the clone and the drop
  of the path-copied nodes are inside the cost, in every map (that is most of an add's or a
  remove's cost: imbl clones the nodes on the path, keys included).
- **Keys:** the list holds 4,000 `testkit::numbered` masternodes: 16,500 keys (collateral,
  owner key id, operator key, service, an Evo node's platformNodeID). (a) is the list's own keys
  (misses are those of 4,000 more masternodes). (b) adds the fixture's 512 services to the list
  (`2600:` + 8 colliding bytes + 6 index bytes, port 19999, 18 bytes, as `UniqueKey::service`
  builds them) and runs the operations on them (misses are 512 more of the family). The run
  checks that the 512 keys hash to one Fx value (and to 512 SipHash values) with this crate's
  layout and `Hash` impl. (c) is (b)'s shape with the colliding bytes replaced by the index,
  which separates the key length from the collisions.
- **Reproduce:** `RUSTC_WRAPPER= cargo bench -p dash-dml --bench unique_map_compare -- --compare`
  (about 6 minutes at a load of 150; needs `perf`).

Instructions per operation, median (min-max) of five runs of 10⁵ operations:

| keys | map | probe | add | remove |
|---|---|---|---|---|
| (a) normal | SipHash-1-3 (shipped) | 412 (411-412) | 12,025 (12,010-12,085) | 11,975 (11,960-11,997) |
| (a) normal | FxHasher | 186 (186-186) | 11,604 (11,601-11,604) | 11,542 (11,540-11,542) |
| (a) normal | OrdMap | 859 (859-859) | 15,967 (15,966-15,977) | 13,878 (13,876-13,888) |
| (b) 512 Fx-colliding | SipHash-1-3 (shipped) | 388 (387-388) | 12,063 (12,027-12,149) | 12,042 (11,965-12,082) |
| (b) 512 Fx-colliding | FxHasher | 13,756 (13,756-13,756) | 269,753 (269,752-269,754) | 253,636 (253,636-253,638) |
| (b) 512 Fx-colliding | OrdMap | 946 (946-946) | 23,231 (23,228-23,241) | 21,724 (21,722-21,736) |
| (c) same shape, not colliding | SipHash-1-3 (shipped) | 388 (388-388) | 12,104 (12,032-12,133) | 12,020 (11,980-12,066) |
| (c) same shape, not colliding | FxHasher | 180 (180-180) | 11,661 (11,660-11,661) | 11,590 (11,588-11,590) |
| (c) same shape, not colliding | OrdMap | 946 (946-946) | 23,230 (23,230-23,239) | 21,725 (21,724-21,736) |

The largest number of keys sharing one hash value (the 32 bits imbl uses), and the comparisons
one `get` made over every listed and unlisted key (a miss usually makes none, so the mean is
about 0.5):

| keys | map | max keys sharing a hash | max comparisons per get | mean |
|---|---|---|---|---|
| (a) normal | SipHash | 2 (worst of 20 keys; 1 in an earlier run) | 2 | 0.50 |
| (a) normal | Fx | 1 | 1 | 0.50 |
| (a) normal | OrdMap | n/a | 18 | 14.14 |
| (b) 512 Fx-colliding | SipHash | 1 (worst of 20 keys) | 1 | 0.50 |
| (b) 512 Fx-colliding | Fx | 512 | 512 | 384.25 |
| (b) 512 Fx-colliding | OrdMap | n/a | 17 | 15.18 |
| (c) same shape, not colliding | SipHash | 1 (worst of 20 keys) | 1 | 0.50 |
| (c) same shape, not colliding | Fx | 1 | 1 | 0.50 |
| (c) same shape, not colliding | OrdMap | n/a | 17 | 15.18 |

Criteria (§5.5):

- **SipHash on (b) within 2x of SipHash on (a): PASS.** The ratios are 0.94x (probe), 1.00x
  (add) and 1.01x (remove); against (c), the same shape without collisions, 1.00x in all three.
- **SipHash <= 1.2x Fx on (a): replaced by DEC-182.** It failed for the probe, 2.22x (412 against
  186), and passed for add and remove, 1.04x each, because the path copy dominates them. The
  probe's extra is the keyed hash alone, about 226 instructions for a key of 18 to 48 bytes.
  DEC-182 drops this bound and keeps only the criterion above, adversarial keys within 2x of
  normal keys.

Conclusion: the shipped SipHash does not slow down under the fixture, while Fx is 74x slower on
a probe and 23x and 22x slower on an add and a remove than on (a), with a 512-key bucket, so a
fixed hasher is not an option for these keys. SipHash misses the 1.2x bound on an isolated probe
(2.2x, 226 instructions) and meets it on the mutations; `OrdMap` is immune to the fixture but is
2.1x-2.4x SipHash on a probe and 1.16x-1.93x on a mutation. **DEC-182: SipHash stays.** The
bound is adversarial within 2x of normal, which SipHash meets (a probe on (b) is 388 against 412
on (a)); the 1.2x-of-Fx bound is withdrawn, so the probe's 226 extra instructions are accepted.

## Core files mirrored

| Rust | Core (v24.0.0-rc.3, DEC-82) |
|---|---|
| `types` | `evo/dmnstate.h` (`CDeterministicMNState`), `evo/deterministicmns.h` (`CDeterministicMN`, `ImmerHasher`, `GetCounts`) |
| `list`, `unique` | `evo/deterministicmns.h` (`CDeterministicMNList` lookups, `mnUniquePropertyMap`) |
| `diff` | `evo/deterministicmns.h` (`CDeterministicMNListDiff`), `evo/dmnstate.h` (`CDeterministicMNStateDiff`) |
| `store` | `evo/evodb.h` (`WriteDerived`), `llmq/blockprocessor.cpp` (the mined-commitment index), `llmq/snapshot.cpp` (`StoreSnapshotForBlock`) |
| `codec` | `evo/snapshot.h` v24 (the canonical list and diff, `CanonicalMNListHash`), `evo/deterministicmns.h` (`SerializationOp`s read by `core_disk`) |
| `store::resolve` | `evo/deterministicmns.cpp` (`GetListForBlockInternal`, `CleanupCache`) |
| `creditpool`, `mnhf` | `evo/creditpool.h` (`CCreditPool`), `util/ranges_set.h`, `evo/mnhftx.h` (`Signals`) |

Deviations are listed in `dash-evo/API.md` §5.
