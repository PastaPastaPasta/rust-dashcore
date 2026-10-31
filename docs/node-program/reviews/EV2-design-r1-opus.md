# EV2 design review, round 1 (Opus)

| | |
|---|---|
| Subject | `designs/EV2-dml.md` (reconciled EV2a + EV2b design), including its §9 reconciliation log; DEC-50 and the member-source ruling taken as given |
| Reviewer | Opus 5.5, read-only |
| Core checked | `v23.1.8` and `v24.0.0-rc.3` in `~/workspace/dash`, extracted with `git show <tag>:<path>` to `/work/scratch/ev2-rev-opus/{v23,v24}/` (file names have `/` replaced by `_`). The files read: `evo/{deterministicmns.*,specialtxman.*,dmnstate.h,providertx.*,smldiff.cpp,simplifiedmns.cpp,snapshot.*,evodb.h,netinfo.h}`, `masternode/{payments,meta}.cpp`, `llmq/utils.cpp`, `rpc/{evo,masternode,governance}.cpp`, `bls/bls.h`, `validation.cpp`, and immer `detail/hamts/{champ_iterator,node,bits}.hpp` |
| Live data | mainnet oracle (`getblockhash`/`getblock 2` only, the pure reads; concurrency 1). Script: `/dev/shm/ev2-rev-opus-tmp/qcscan.py` (deleted after the review; reproduced in M1) |
| Verdict | **APPROVE-WITH-CHANGES**: 0 blockers, 1 major, 6 minors, 7 nits |

The design is accurate where it counts. I re-derived every step of `RebuildListFromBlock` and `ProcessBlock` at both pins and found no ordering or semantic error in §3.2–§3.8. The unique-map, iteration-order, codec and removal arguments hold. The one major finding is about evidence, not design: the EV2b merge-gate replay window contains no non-null quorum commitment, so it never exercises PoSe on real data.

---

## Answers to the six review questions

### 1. Unique-property map keyed by the pre-image: equivalent

- **Never iterated or serialized.** At both tags `git grep mnUniquePropertyMap` outside `evo/deterministicmns.{h,cpp}` finds only a v24 comment (`dmnstate.h:264`) and a test comment. Inside, the map is used only by `count`/`find`/`set`/`erase`, the rollback copy, `IsEqual` (`deterministicmns.h` v23:454 / v24:492) and `operator==`. Both comparisons are content equality, which no injective re-keying changes. `IsEqual` is used only by evodb verify/repair (`deterministicmns.cpp` v23:1333, 1403) and by tests. `CDeterministicMNList::Unserialize` rebuilds the map through `AddMN` (v24 `deterministicmns.h:247-257`), so it is never stored. The map's own `ImmerHasher` (ReadLE64 of the SHA256d) is invisible.
- **Equivalent on every collision class.** In Core, two values share a slot iff `SHA256d(ser(a)) == SHA256d(ser(b))`, which (barring a SHA256d collision) means `ser(a) == ser(b)`. Keying by `ser(·)` itself gives the same relation. The same holds for different lengths: equal bytes means equal length, so the 20-byte, 36-byte, 18-byte and 48-byte classes behave as they do in Core. `GetUniquePropertyHash` is byte-identical at both tags (v23 `deterministicmns.h:488-510`, v24 `:526-548`), including the ADDRv2 switch for services that are not AddrV1-compatible. `DomainPort` writes the same bytes at both tags: `READWRITE(m_addr)` in v23 and `LIMITED_STRING(m_addr, 253)` in v24 differ only on read, and DomainPort is reachable only with V24.
- **The operator key.** `SerializeHash(CBLSLazyPublicKey)` goes through `Serialize(s, bufLegacyScheme)` (`bls.h:446-464`). For every key that can be in a state (valid, non-malleable, scheme equal to `nVersion == 1`), that is the 48 bytes in the stored encoding, as §2.2 says. `HasOperatorKeyUnderAnyScheme` (v24 `deterministicmns.h:471-482`) probes both encodings through `Set(pubkey, legacy)`; the Rust probe needs BS11's `reencode` for both.
- **Equivalence depends entirely on the pre-image encoder being byte-exact**, and the proposed test does not check that (m3). Null-value handling also needs to be stated for every property type (m4).

### 2. CHAMP iteration order: complete for every observable

I enumerated every `ForEachMN`/`ForEachMNShared` call and every direct `mnMap` iteration at both tags (outside tests and bench):

| Consumer | Order-sensitive? |
|---|---|
| `BuildSimplifiedDiff` (`smldiff.cpp` v23:128,143 / v24:127,144) | **yes**: in the design |
| `protx list` (`rpc/evo.cpp` v23:1476,1506 / v24:1709,1740), `masternodelist` (`rpc/masternode.cpp` v23:612 / v24:666) | **yes**: in the design |
| `GetMNByOperatorKey` (`ranges::find_if(mnMap)`, `deterministicmns.cpp` v23:121 / v24:95), consumed by `active/masternode.cpp:134` | **yes, first match**: in the design |
| `GetMNPayee` / `GetProjectedMNPayees` Evo scans (last match) | unique by I-P (see m1 for imported lists) |
| `CDeterministicMNList::Serialize` (`dmn_S3`) | import only |
| `BuildDiff` (iterates `to`; removals iterate `from` with an early break into a `std::set`) | no: `added` is sorted; `removedMns` is a set and the break fires only once it is complete; `updatedMNs` is an `unordered_map` (v24 salted per process) |
| `GetProjectedMNPayees` main loop, `GetMNPayee` minimum, `DecreaseScores`, confirmation loop, streak reset, `GetCounts`, `to_sml` | no (strict total order, or independent updates) |
| `llmq/utils.cpp` scoring (v23:139 / v24:143), `MnsNotUsedAtH` (v23:389 / v24:397), `BuildQuorumSnapshot` | no: `CalculateQuorum` sorts by (score, collateral) (v24:160-171) |
| `llmq/net_quorum.cpp:691` (v24; `active/quorums.cpp:93` in v23) | no: `std::sort` |
| `governance.cpp` v24:561, `rpc/governance.cpp:554` | no: `std::map` |
| `coinjoin/client.cpp` v24:1279 | no: `Shuffle` |
| `init.cpp` coin-cache warmup, `node/interfaces.cpp` (GUI) | not observable |
| `snapshot.{h,cpp}` (v24) | no: sorted, or validation only |

Payee, PoSe and the unique-property paths do no first-match iteration over the MN map. PoSe walks the member vector, which LQ1 supplies in Core's member order, and the unique checks are map probes. The algorithm in §2.7 matches `champ_iterator.hpp`: a node's values (in slot order) come before descent into its children (`step_down`/`step_right`), and `max_depth = ceil(64/5) = 13`. The reconciler's oracle evidence (`/work/scratch/ev2-rec/*.log`) covers lists of 4,753 and 542 entries and the deletion subset. Two corrections to the quirk-41 text are in n1: collision order is newest-first, and the order depends on the host's word size.

### 3. Diff and snapshot byte equivalence: confirmed

- v24 `CDeterministicMNListDiff::Serialize` (`deterministicmns.h` v24:639-659) writes `s << addedMNs` (CompactSize plus each MN), then the `updatedMNs` ids sorted, then `removedMns` from a `std::set`. `BuildDiff` sorts `addedMNs` by internalId (v24:362-366), and internalIds are unique. `SerializeCanonicalMNListDiff` (`snapshot.h` v24:304-328) sorts by `(internalId, proTxHash)`, which gives the same sequence, then sorted updated, then the set. Both use `SER_DISK, CLIENT_VERSION` (`snapshot.cpp:152`, evodb). **So v24 `dmn_D4` equals the canonical diff for every diff that `BuildDiff` produces.**
- **v23 for pre-V24 states.** `CDeterministicMN` serialization is identical at both tags apart from the new `HasInternalId` accessor. The state serialization differs only inside `nVersion >= ExtAddr` (payouts/shares/early fields instead of `scriptPayout`). The state-diff member tuple differs only by the four V24 members inserted between `scriptPayout` and `scriptOperatorPayout`, whose bits are never set before V24. `MnNetInfo`, `NetInfoSerWrapper` and `NetInfoEntry` serialization are identical on write. The `pubKeyOperator` diff rule is a point comparison in v23 and `GetHash()` in v24 (v24 `dmnstate.h:259-268`). The two differ only when a key changes encoding but keeps its point, and before V24 that cannot happen: I traced every v23 writer of `pubKeyOperator`/`nVersion` (ProRegTx state ctor; ProUpRegTx with a changed key, where `ResetOperatorFields` sets version 1 so the v23 ternary always picks the payload version; ProUpRevTx; ProUpServTx only under `is_v24_deployed`). The `CLIENT_VERSION` values differ (230108 vs 240000), but both satisfy `== CLIENT_VERSION || >= DMN_TYPE_PROTO_VERSION`, and neither has the ADDRV2 bit, so the bytes are identical. The only v23 difference is the `unordered_map` order of `updatedMNs` (v23:591-605), which is import-only. Confirmed.
- **Application order.** The §2.8 argument holds: EV1 checks every claimed property against pprev, so a valid block cannot move a pprev-held property between two surviving MNs. The only property a block can move is the one a ProRegTx replacement frees, and `ApplyDiff` applies removals first. I also tried several in-block chains (claim, release and re-claim of a property that was free at pprev); none produces an apply-order conflict.

### 4. Order of operations in `build_new_list_from_block`: correct at both pins

I compared the §3.2 table line by line against v23 `specialtxman.cpp:185-520` and v24 `:309-795`. Steps 0–7 are right, including:
- The confirmation loop runs over `prevList` with banned MNs included.
- `DecreaseScores` touches only valid MNs and runs before every ProTx and QcTx.
- PoSe adds penalty to banned MNs too, and `CalcMaxPoSePenalty` runs on the partly rebuilt list, so each `CalcPenalty(66)` is re-evaluated against the registrations so far.
- QcTx is processed in block order alongside the ProTxs.
- The spend pass runs over every transaction from `vtx[1]` on, after all registrations.
- The payee bump uses `GetMN` (not `GetValidMN`).
- The streak reset runs over `newList2` with the post-MN_RR payee included.

The v24 probe positions are right (`HasOperatorKeyUnderAnyScheme` on the new key for ProReg and ProUpReg, on the *old* key for the ProUpServ migration), and so is the pre-V24 equivalence of v24's `SetStateVersion` and `ApplyProUpServTx` with v23's inline code. I traced ProUpReg with the key changed and unchanged, ProUpRev, and ProUpServ, and v24 before V24 equals v23 in every case.

- **The EVO-15 argument (§3.10) holds.** v24's `CProUpServTx::IsTriviallyValid` rejects a bad `nType` for every version (`providertx.cpp:420-422`, called from `GetValidatedPayload` at `specialtxman.cpp:1117`), and `CheckProUpServTx` rejects a mismatch (v24:1347-1350), both before the build. Inside EV2b only netinfo and then hash are reachable, which matches v23's order. The test vector in §3.10 is wrong, though (m2).
- **Exceptions.** Every throw inside the build comes from `AddMN`/`UpdateMN`/`RemoveMN`/`PoSePunish`, or is UB (`validMembers[i]`), and the design maps each. The asserts (`penalty > 0`, `percent > 0`, `PoSeDecrease`, `SetHeight`) cannot fire. Quirk 42's asymmetry is real: ProRegTx re-checks netinfo, owner and operator, ProUpServ re-checks netinfo only, and ProUpReg before V24 re-checks nothing.
- **Hardfork gates.** V19 gates only the payee continuation (projections do not check V19; the design has this right). The global BLS flag does not reach the DML: every lazy key in a state or payload carries an explicit scheme, `ResetOperatorFields`' default key is null (serialized as zeros, hash identical), and the flip itself happens after the build (v24 `specialtxman.cpp:982-988`). Nothing in the DML is gated on V20. MN_RR and V24 are handled as the design says.
- **`ProcessBlock`.** Confirmed: v23 runs `ResetPlatformBan` before the evodb `Write` (v23:673-688), v24 runs it after `WriteDerived` and the snapshot write (v24:735-750), and both run it before `bad-dip3-enf-block`. `updatesRet` is filled only on changes in v23 (v23:707-709) and always in v24. A1 §2.10.1 already fixes "apply regardless of the result", so the effect also survives later `bad-cbtx-mnmerkleroot`, `bad-cb-payee` and script failures, as Core's in-memory meta store does.

### 5. Removals: both safe

- **Typed collateral mirror.** `GetMNByCollateral` is `GetUniquePropertyMN(outpoint)` (v24 `deterministicmns.cpp:104-107`), so a probe of the pre-image map is Core's lookup, collision behaviour included. Dropping the mirror is actually *more* faithful: in the ProRegTx replacement step Core does no `collateralOutpoint ==` check (v24:389-401), so a 36-byte TORv3/I2P service slot that collides with the outpoint would remove the MN that owns the service. A typed mirror would not do that. (This is unreachable in practice because the attacker would need a txid with a chosen 32-byte value; it is not a security issue.)
- **`last_payee`.** The proof is sound. `pay_height ≤ list.height` for every MN, because last-paid, revived and registered heights are all at most the list height. The MN with `last_paid == H` has `pay_height == H`, because its revived height cannot exceed H. The queue holds only valid MNs, and Core scans only valid MNs. The I-P proof is right for lists built from genesis. The only gap is lists that enter by import (m1).

### 6. Verification: a payee bug is caught, a PoSe or ordering bug is not (M1)

`CanonicalMNListHash` per block includes `nLastPaidHeight`, so a wrong regular-MN payee shows up in the very block that pays it, inside the L2 window. PoSe is a different matter: punishment, bans, decay, and decay-versus-punishment ordering are absent from the window entirely, and so are the evonode continuation and MN_RR. Details under M1.

---

## Findings

### Major

**M1. The EV2b merge-gate replay window (§6.4) contains no non-null quorum commitment, so PoSe is never exercised on real data before M4.**
- *Where:* §6.4 "EV2b merge gate" and §7 EV2b-2 acceptance. The design leaves it open: "Whether the first non-null QcTx commitments fall inside it is read off the oracle's blocks now".
- *Evidence (mainnet oracle, `getblock 2`, one full 24-block DKG interval sampled every 4,800 blocks over 1,028,160–1,078,159, then every 2,400 blocks after it):* every QcTx in the window is a null `LLMQ_50_60` commitment (`signersCount 0`; for example at 1,030,000, 1,060,000 and 1,078,000). The first non-null commitments appear between 1,080,552 and 1,082,963 (1,082,963: 43 signers, 43 valid of 50, which already causes PoSe punishments), and they come every interval after that (1,102,164: 46 signers, 45 valid).
- *Failure scenario:* an EV2b bug confined to the PoSe path passes every real-data gate EV2b is merged on. The only pre-M4 evidence is L0 (synthetic regtest scenarios) and L1 `diff_dml_apply`, which depends on the O2a-EV member seam that does not exist yet. Example bugs: decay applied after a same-block punishment; `calc_penalty` cached once per block instead of per call; a ban height taken from `prev.height`; `pose_punish` skipping banned MNs (quirk 24); a missed revive interaction. The evonode continuation (V19 ≤ h < MN_RR) and the MN_RR switch are likewise covered only synthetically until CH7a's full replay. The twin cannot catch a shared semantic misreading.
- *Suggested fix:*
  1. Extend the EV2b-2 L2 window to start at 1,028,160 and end at or after **1,128,159**. Starting at DIP3, it still needs no imported state. It then covers the first non-null QCs (from about 1,082,963), PoSe punishments and bans, decay, at least one revive, and the 1,088,639 boundary.
  2. Make two windows started from imported snapshots part of EV2b acceptance (EV2b-2, or EV2b-4 at the latest, before CH7a). Start them from the `getcanonicalmnlist` vectors EV2a already produces at 1,899,071 and 2,128,895: one window inside [V19, MN_RR) that has active evonodes (continuation and streak), and one across the MN_RR activation.
  3. Harvest the recorded `QuorumMemberSource` on an **O7 replica**. `quorum info` is not a pure read on the live oracles (manager ruling, LQ1 rev1).
  4. Replace the conditional sentence in §6.4 with the measured fact.

### Minor

**m1. I-P is claimed as a decoder rule that §2.9 does not have, and either reading breaks something.**
- *Where:* §2.4 I-P ("The canonical decoder rejects a second (§2.9)"), §2.9 decoder rules, §3.4 `payee(r)`.
- *Scenario:* Core's `UnserializeCanonicalMNList` (v24 `snapshot.h:249-298`) accepts a list with two valid Evo MNs at `nLastPaidHeight == nHeight`.
  - If our decoder adds the rule, `diff_mnlist_codec` (accept/reject parity) disagrees with Core on mutated lists.
  - If it does not, `payee()` on such an imported list returns the first match in a top-run scan, where Core returns the *last match in CHAMP order* (`deterministicmns.cpp` v24:171-179). `projected_payees` has the same issue. The L1 synthetic-list generator can also produce such states and cause false mismatches.
- *Fix:* drop the decoder rule. In `payee`/`projected_payees`, collect the top-run matches (r is tiny) and pick the last one in `core_order()` when there is more than one, so the result is correct whether or not I-P holds. Keep I-P as a debug and xcheck invariant for lists built from blocks only.

**m2. The EVO-15 test vector in §3.10 is wrong under `V24_0`.**
- *Where:* §3.10 table, "Evidence" cell: "a ProUpServTx with unknown `proTxHash` and `nType = 7` → `bad-protx-hash` at block level under both profiles".
- *Core:*
  - v24 rejects it in EV1 with **`bad-protx-type`**, because `GetValidatedPayload` → `IsTriviallyValid` (`providertx.cpp:420-422`, `specialtxman.cpp:1117`) runs before the hash lookup (`:1339-1341`).
  - v23 rejects it in EV1 with `bad-protx-hash` (`specialtxman.cpp` v23:1082-1085), because v23's `IsTriviallyValid` has no `nType` check (v23 `providertx.cpp:165-190`).
  - Neither verdict comes from EV2b.
- *Fix:* expect `bad-protx-type` under `V24_0` and `bad-protx-hash` under `V23_1`, both attributed to EV1. To hit EV2b's own order, use a block where a ProRegTx replaces MN X (collateral reuse) and a later ProUpServTx targets X: the result is `bad-protx-hash` from the build under both profiles.

**m3. The unique-key equivalence test (§2.2, §6.2) is tautological.**
- *Where:* "A unit test asserts … `UniqueKey(a) == UniqueKey(b) ⇔ SHA256d(pre(a)) == SHA256d(pre(b))`".
- *Problem:* that holds for any encoder. What matters is that `UniqueKey::x(v)` is exactly `ser(v)` under `SER_GETHASH, PROTOCOL_VERSION [| ADDRV2_FORMAT]`. A wrong ADDRv1/ADDRv2 choice (`IsAddrV1Compatible`: IPv4, IPv6 and internal are v1; TORv3, I2P and CJDNS are v2), a wrong DomainPort length prefix or port byte order, or the wrong BLS encoding would change collision classes, and the twin would share the bug (§3.4).
- *Fix:* assert `SHA256d(UniqueKey::x(v).bytes()) == Core GetUniquePropertyHash(v)` for every property type and every collision class, using vectors Core produced: a small `oracle_unique_property_hash` entry in O2a-EV, or a one-off vector dump. Include IPv4, IPv6, CJDNS, TORv3, I2P, DomainPort, keyID/uint160, and legacy/basic BLS.

**m4. Null-value semantics of the unique map are incomplete.**
- *Where:* §2.2 ("Null values are never inserted … skipped by the callers") and §2.5 `try_add`/`remove`.
- *Core:* `AddUniqueProperty`, `DeleteUniqueProperty` and `UpdateUniqueProperty` compare against `T{}` for every type (v24 `deterministicmns.h:552, 572, 604`). `Add` and `Delete` return **false** on a null value. So `AddMN` with a netinfo entry equal to `CService{}`/`DomainPort{}`, or with a null non-shared owner, throws "duplicate …"; `RemoveMN` of such an MN throws "Can't delete …". The callers skip only the operator key and the platformNodeID.
- *Scenario:* valid blocks cannot reach this (EV1 validates services and keys), but `fuzz_apply_diff`, the `mnlist_model` proptest and the L1 synthetic generator can. A Rust `try_add` that inserts or skips nulls would diverge from the twin and the oracle.
- *Fix:* state in §2.5, per property type, that a null value makes add or delete fail with Core's error class (and that `update` skips nulls on either side, as `UpdateUniqueProperty` does), and add a test.

**m5. The EV2a L2 gate cannot pass at 1,028,159 as written.**
- *Where:* §6.4 "EV2a gate … (1,028,159; …) Our decoder reads the bytes, our encoder reproduces them".
- *Core:* `GetListForBlock` of a pre-DIP3 block is the default list (hash 0, height −1; v24 `deterministicmns.cpp:830-832`). `UnserializeCanonicalMNList` throws "negative canonical MN-list height" (v24 `snapshot.h:256`), and `ValidateCanonicalMNInvariants` rejects it too (`snapshot.cpp:106`). Our ported decoder must reject it, and our encoder (§2.9: "applies `ValidateCanonicalMNInvariants` first") must refuse it.
- *Fix:* at pre-DIP3 heights compare only `CanonicalMNListHash`, through an unvalidated hash path (Core's `CanonicalMNListHash` does not validate). Alternatively, replace 1,028,159 with 1,028,160.

**m6. The `QuorumMemberSource` shape disagrees with the current LQ1 design.**
- *Where:* EV2 §2.11 has `trait QuorumMemberSource<E: MnEntryView> { fn members(…) -> Result<QuorumMembers<E>, LlmqError>; }` and maps `UnknownLlmqType` to `OutOfDomain`. `designs/LQ1-quorum-members.md` §2.6 (lines 342-361 of the working-tree file) has a non-generic trait returning `Result<Arc<[Arc<Dmn>]>, MemberError>`, with `UnknownLlmqType` → `DmlException::Members`, although its §2.4 also defines `LlmqError`.
- *Fix:* both mappings end in `failed-procspectxsinblock`, so the semantics agree, but EV0 can land only one signature. The EV2 and LQ1 designs should cite one text, which DEC-50(10) intends. DEC-50(10) also cites a "DEC-48 ruling", but DEC-48 is P2a's decision row; the member-source ruling is the LQ1-review M1 ruling and has no DEC row. Record it, or fix the citation.

### Nits

- **n1 (quirk-41 wording).**
  - *Collision order.* Collision-node order is **newest first**, not insertion order: `make_merged(shift, new, …, old, …)` reaches `make_collision(new, old)`, and `copy_collision_insert` prepends (immer `node.hpp:384-405, 982-1007`). Reloading `dmn_S3` re-inserts in iteration order and so reverses it. Core nodes therefore disagree among themselves, and the declared deviation (§3.11 item 3) stands. If we want to match the majority of nodes that built their lists from genesis, descending internalId is closer to them than proTxHash order.
  - *Host word size.* Core's order depends on the host: `hash_t = std::size_t` (`bits.hpp:23`), and the release HOSTS include `arm-linux-gnueabihf` (`contrib/guix/guix-build:84`). On 32-bit builds `ReadLE64` is truncated to 32 bits and the trie is 7 deep, which gives a different `mnlistdiff` order.
  - *Fix:* state "64-bit hosts" in quirk 41 and in the memory note `dash-mnlist-champ-order` (which also says "insertion order"). dash-spv must not depend on the order.
- **n2.** v24's shared-collateral spend check tests the raw `tx.nType != TRANSACTION_PROVIDER_DISSOLVE` with no `IsSpecialTxVersion()` (`specialtxman.cpp:739`). A valid block cannot exploit this (`ContextualCheckTransaction` forbids a non-zero `nType` below version 3, `validation.cpp` v24:375-377). The oracle's `oracle_dml_apply` skips that check, though, so §3.5 should specify the raw `nType` for fuzz parity.
- **n3.** `get_by_operator_key` with an invalid or default query key matches the first MN whose operator key is null: Core's `Get()` returns the same `invalidObj` for both (`bls.h:492-512`). Specify the behaviour, or guard it in the API.
- **n4.** §3.2.1 step 9's "a duplicate collateral held by the same MN that step 4 could not see" cannot happen. Step 4's `GetMNByCollateral` removes the owner of the slot whatever its type, so the collateral slot is always free before `AddMN`. Reword, and note the cross-type removal as part of quirk 23/25.
- **n5.** §3.11 item 1 and §3.9 overstate v24. The DML's own store faults in v24 (`ApplyDiff` `runtime_error`, `BlockDataUnavailableError` from `GetListForBlockInternal`) are not `EvoDbInconsistencyError`, so v24 also maps them to `failed-procspectxsinblock`. Only the credit-pool and MNHF faults take the `state.Error` path. The ruling stands; the stated parity with v24 does not.
- **n6.** The `UniqueMap` fixed-key FxHasher lets anyone construct colliding service addresses offline. Collision buckets are bounded by the attacker's own MN entries, so the cost is negligible, but a keyed hasher (SipHash with a per-process random key) costs nothing observable, because the map is never iterated.
- **n7.** `projected_payees(r, n: usize)` cannot express Core's `nCount < 0 → {}` (v24 `deterministicmns.cpp:198-200`). Note that RP2a owns the negative-count case.

---

## What was verified and how

- Every Core line cited in §3.2–§3.8 and Appendix A for `RebuildListFromBlock`, `ProcessBlock`, `UndoBlock`, `GetListForBlockInternal`, `AddMN`/`UpdateMN`/`RemoveMN`, the payee and PoSe code, `BuildDiff`/`ApplyDiff`, the state and state-diff codecs, and the canonical codecs, at both tags. `diff` of the payee and PoSe block across the tags (v23:157-360 against v24:132-335) shows only `irange` spelling and a cast.
- The ConnectBlock order at both pins: `ProcessSpecialTxsInBlock` runs before `UpdateCoins` and the payee checks (v24 `validation.cpp:2663, 2767, 2846-2864`). DML exceptions are caught at v24 `specialtxman.cpp:989-996`.
- The immer CHAMP iterator and the collision-node construction (`champ_iterator.hpp`, `node.hpp`, `bits.hpp`). The vendored immer is identical at both tags (design header; not re-checked).
- The live-data QC scan on the mainnet oracle (M1). I ran no Rust tests (design review, no code).
- No suspected Dash Core vulnerability.

## Verdict

**APPROVE-WITH-CHANGES.** M1 must be resolved before EV2b-2 merges, by extending the window and adding the imported windows. Fold m1–m6 into the design's next revision; m2, m3 and m5 are one-line spec fixes.
