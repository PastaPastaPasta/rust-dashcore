# dash-dml API: the node-only half of EV0

EV0 landed one `dash-evo` crate; the EV addendum (`docs/node-program/designs/EV-addendum-SB0.md`
rev6, DEC-122) splits it for SB0 (rev6.1, DEC-119) into the shared `dash-evo` and this node-only
crate. Names are EV0's; only paths change unless the table says otherwise. EV0's full requirement
map (EV2, LQ1, A1, ST3a, CS0, the BLS ruling), its deviations and its decisions are in
[`dash-evo/API.md`](../dash-evo/API.md), whose §0 maps both crates; this file lists what lives
here.

Inputs: those of `dash-evo/API.md`, and the EV addendum §1.1, §1.2, §2.2, §2.3, §2.6 and §4.1.

## 1. Items (EV addendum §1.2)

| EV0 item (`dash_evo::…`) | Here | Change |
|---|---|---|
| `dml::{Dmn, MnHot, MnCold, MnList, Counts, PayeeKey, Provenance, KeyId, OperatorKey, ScriptBytes, UniqueKey, UniqueMap, UniqueSlot, SameRecord, MnListDiff, MnStateDiff, MnListDiffRecord, DmlDelta}` | `dash_dml::*` (flattened) | moved |
| `dml::{ProTxHash, MnType, RawMnType}` | `dash_evo::types` | moved there; re-exported here |
| `store::{StoreError, DmlReader, EvoStore, EvoDelta, MinedCommitmentRecord, MemEvoStore, MemEvoView}` | `dash_dml::store` | moved; `EvoStore: DmlReader + MinedCommitmentReader<Error = StoreError>` |
| `store::{MinedCommitmentReader, MinedFamily, MinedIndexEntry, MinedCommitment, base_through}` | `dash_evo::llmq` | moved there; the reader gains `type Error: From<BrokenInvariant>` |
| `llmq::MemberError` (two variants) | `dash_dml::store::NodeMemberError` | EV0's enum, node-side; `From<MemberError<StoreError>>` maps `Missing` to `Store(Inconsistent)` (D4) |
| `creditpool::{CreditPool, RangesSet}`, `mnhf::MnhfSignals` | `dash_dml::{creditpool, mnhf}` | moved |
| `testkit::{cold_state, dmn}`, `MnList::insert_for_tests` | `dash_dml::testkit` | moved; `MemChain` and `pro_tx` stay in `dash_evo::testkit` and are re-exported here |
| `tests/store.rs`, `tests/consumers/{ev1,ev2a,ev2b,ch7a}.rs` | `tests/` | moved |
| `tests/consumers/lq1a.rs` | `tests/consumers/lq1a.rs` | the view traits are `dash_evo` source; the per-call `AttemptRows` is a view over an attempt-owned overlay (§2.6) |
| `imbl = "=7.0.2"` (DEC-50(6)) | `Cargo.toml` | the pin moves here; `dash-evo` no longer depends on `imbl` |

## 2. New here (EV addendum §1.3, §2.2)

| Item | Serves |
|---|---|
| `impl MnEntryView for Dmn`, `impl MnListView for MnList` (`Entry = Arc<Dmn>`, `Scratch = MnList`), `impl ScratchMnList for MnList` | LQ1 §2.2 over the node's list (`views.rs`); `try_add` is EV2a's `MnList::try_add` |
| `impl From<BrokenInvariant> for StoreError` | a broken ancestry invariant is `Inconsistent` |
| `store::DmlMemberSource<'a>` | `dyn QuorumMemberSource<Entry = Arc<Dmn>, Error = StoreError> + 'a`, what node callers hold as `&DmlMemberSource<'_>`; the explicit lifetime lets a source borrow its attempt (§2.6; Sol r3 R3-M1) |
| `store::NodeMemberError { Store, UnknownLlmqType }` | EV2 §2.11's and LQ1 §2.6's node mapping, unchanged |

## 3. EV2a (EV2 §2, §3.7 erratum; EV addendum §4.1)

| Item | What |
|---|---|
| `MnList::{try_add, update, update_hot, remove, core_order, check_invariants, is_equal}` | the list's mutations with Core's unique-property rules, its invariants, Core's CHAMP iteration order and Core's `IsEqual` (which leaves out the internal ids and the counter) |
| `MnList::diff_to`, `MnListDiffRecord::build`, `MnList::apply_record` | `BuildDiff`/`ApplyDiff` and the stored record: Core's diff plus the counter and the collision-bucket trailer, so a rebuilt list equals the online one |
| `store::{resolve_list, ListCache, NoListCache, LruListCache, CacheHint}` | `GetListForBlockInternal` over the hash-keyed rows; the cache policy is not observable |
| `codec::{encode_canonical_list, decode_canonical_list, canonical_mn_list_hash}` | v24's canonical EvoSnapshot MN-list encoding (`snapshot.h`), with Core's reader checks and texts, and `CanonicalMNListHash` |
| `codec::{encode_canonical_diff, decode_canonical_diff}` | the canonical MN-list diff, with Core's bounded counts and operation budget |
| `codec::{encode_snapshot_record, encode_diff_record, decode_record, DmlRecord, DmlRecordBytes}` | the store's record envelope (version, codec, height, body, bucket trailer) |
| `codec::core_disk::{decode_list_s3, decode_diff_d4}` | Core's evodb list and diff readers, for import (the CHAMP reload order of quirk 41) |
| `codec::NetInfoValidity` (`ContextFreeNetInfo` under `test-utils`) | the hook for Core's network-dependent `netInfo->Validate()`, which is EV1's; the codec never decides it (D12) |
| `testkit::numbered(i)` | a mainnet-shaped MN for benches and large-list tests |
| `benches/mnlist.rs` | the PF0 rows `mnlist-{clone-diff,unique-probe,codec-decode,codec-hash}` |

The Core vectors (`tests/data/codec/`) come from `contrib/node/ev2a-codec-probe`, a program
linked against the v24 libraries; their README records the provenance.

## 4. Tests

- `src/views.rs::an_arc_dmn_is_an_entry`, `tests/consumers/lq1a.rs::an_mn_list_is_an_lq1_list_view`:
  the node's list and entry through the shared views.
- `src/store/mod.rs::a_missing_input_is_a_node_store_inconsistency`: D4's mapping.
- `tests/entry_view.rs`: generic code in another crate reads a `Dmn` through the shared views.
- `tests/consumers/`: EV1, EV2a, EV2b, LQ1a and CH7a against the split API;
  `lq1a.rs::{a_later_member_call_reads_the_row_an_earlier_one_stored,
  a_later_store_overwrites_and_both_are_effects_in_order}` are §2.6 cases 1 (with the per-call
  negative control) and 2.
- `ev2b.rs::an_attempt_bound_source_reaches_the_member_walk` and the `DmlMemberSource` doctests:
  a source borrowing its attempt's view is accepted, and one boxed past the attempt is not
  (`compile_fail`; with the old `'static` alias the consumer test fails with E0597).
- `tests/store.rs`: the in-memory store.
- `tests/reconstruct.rs`: the §3.7 erratum's rule over the histories it builds. Rebuild equals
  online, with the same `CanonicalMNListHash` and snapshot-record bytes, at every connected and
  disconnected block. Each block is rebuilt from each snapshot row at it or an ancestor and from
  none, without a cache, then through one cache in two orders. The histories cover
  register-and-spend, operator-key re-encodings, collision buckets, forks (disconnect, an
  abandoned sibling, reconnect), a restart that writes a new store from the encoded records
  alone, and version-3 transitions. Lists hold only states in Core's state domain
  (`MnListError::VersionHiddenField`: no field the version does not serialize is set, network
  info has the version's form, and a non-default operator key is captured under its version's
  scheme, D17), so these are the states the rule covers.
- `src/codec/tests.rs`: Core's bytes, `CanonicalMNListHash`, reload order, diffs, `ApplyDiff`
  results and reader verdicts on mutated bytes; the record envelope; the fuzz seeds. The ignored
  `imported_lists_match_core_at_full_size` reads full mainnet lists from `DASH_DML_CORE_LISTS`.
- `node-fuzz` `dml_codec_roundtrip`: the canonical, record and evodb codecs round-trip.
