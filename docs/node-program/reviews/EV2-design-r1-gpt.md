# EV2 reconciled design — independent adversarial review, round 1

Reviewed `designs/EV2-dml.md` against Dash Core `v23.1.8` (`728f505583`) and `v24.0.0-rc.3` (`0364ed626e`). DEC-48 and DEC-50 are binding inputs. This review does not reopen DEC-50's regtest V24 deviation, effect ownership, eager-decay decision, or tip snapshot decision. The source branches were read-only; no commits, pushes, or GitHub messages were made.

The indexed payee and PoSe algorithms are mostly sound for lists reachable through validated blocks. The design nevertheless needs changes to its ordering representation, persistence contract, property-map hasher, and differential-test contract before implementation proceeds.

## Findings

### F1 — blocker: full ReadLE64 collisions need mutation history; sorting the key set changes an observable lookup

**Locations:** `designs/EV2-dml.md:306`, `:308`, `:321`, `:705`; also the first-match API at `:283`.

The specification says collision entries use insertion order, calls order a function of the key set, and deliberately sorts full-hash collisions by proTxHash. The first statement is inaccurate and the last two do not reproduce Core.

I compiled a probe against the vendored immer extracted independently from each tag. Both produced this sequence for four unequal keys with the same 64-bit hash:

```text
insert 1:       1
insert 2:       2 1
insert 3:       3 2 1
insert 4:       4 3 2 1
replace 2:      2 4 3 1
erase 3:        2 4 1
readd 3:        3 2 4 1
reverse inserts:1 2 3 4
```

Core's `node.hpp:384-403` prepends inserted collision values; `:484-504` moves a replaced value to the front. `UpdateMN` calls `mnMap.set` even for hot-only changes (`deterministicmns.cpp` v24:588; corresponding v23 code). Thus a payment or decay can change collision order without changing the key set. Deletion preserves the remaining sequence, and decode/reconstruction introduces its own AddMN/UpdateMN history. Both tags have identical immer sources.

The consequence is broader than JSON and mnlistdiff ordering. `GetMNByOperatorKey` returns the **first point match** (v24 `deterministicmns.cpp:93-102`; v23 `:119-127`). The design already recognizes reachable pre-V24 pairs holding one BLS point under different encodings. If their proTxHashes have the same leading 64 bits, Core's mutation history selects one MN while the proposed sorted collision class can select the other. Comparing these classes as sets cannot validate that result. This is an observable masternode identity difference; I am not claiming that the valid-chain payee minimum changes.

**Fix:** retain and reproduce the collision bucket sequence, including insertion, replacement, erase, import, diff application and reorg paths, rather than deriving every order from `mns`. Specify how that metadata survives our snapshots/diffs and which Core reconstruction history is the comparison reference. Alternatively obtain an explicit equivalence exception that includes operator lookup, rather than asserting the deviation is limited to presentation. DEC-50 registers quirk 41 but does not establish that broader exception.

**Verification:** compare operation histories against actual vendored immer, and compare first-match operator results, ordered mnlistdiff additions/deletions, and RPC arrays. A naive trie built from the same key-set specification and canonical-byte equality will miss this. I-U/I-X/I-P do not check mutation history. Scratch evidence: `/work/scratch/ev2-rev-gpt/champ_probe.cpp`, `champ-v23`, `champ-v24`.

### F2 — major: the stored diff cannot reconstruct the online registration counter

**Locations:** `designs/EV2-dml.md:330`, `:352`, `:393`, `:408`, `:731-732`, `:822`.

Consider a non-snapshot block that registers an MN and then spends its collateral in an ordinary transaction. The prescribed order adds and removes the MN, but `total_registered` advances permanently. `diff_to` observes no surviving addition for that MN. Neither the canonical diff nor the proposed envelope records the resulting counter. Applying the stored diff to the previous list therefore loses the counter advance.

A probe linked against the existing v24 oracle-shim Core libraries exercised actual AddMN, RemoveMN, BuildDiff, ApplyDiff and canonical serialization:

```text
online count=1 restored count=0 diff changes=0 canonical equal=0
next online id=1 next restored id=0
```

An existing paid MN can add a payment update to that diff without restoring the missing counter. A trailing newly registered MN that disappears also loses its counter advance when other additions survive. Snapshot cadence and atomic B1 recovery cannot recover information absent from the diff. The approved tip snapshot helps the flushed active tip, but not historical or disconnected lists reconstructed from `mn_diff`.

This metadata issue is inherited from Core's diff representation, not evidence that Core changes its payment consensus. Core explicitly treats internal IDs and the counter as non-deterministic in `IsEqual` (v24 `deterministicmns.h:497-511`; v23 equivalent). EV2 makes the stronger promise that a list has one content regardless of reconstruction path and includes these fields in its canonical hash. Those promises cannot both hold with the proposed record. Subsequent registration IDs, canonical hashes and byte-identical rewrite checks can differ after reconstruction.

**Fix:** specify the counter/provenance contract explicitly. If preserving the online list is required, store the post-block counter alongside the canonical diff in our versioned record, restore it on application, and establish the corresponding oracle provenance for canonical comparisons. Keep Core's raw diff codec separate from the complete internal reconstruction record. If Core's path-dependent metadata is intentionally reproduced instead, revise the equality/hash assertions and obtain the necessary design ruling.

**Verification:** add register-and-spend and multiple-replacement blocks between snapshots; evict all caches; reconstruct every affected height; restart, disconnect/reconnect, and make the next registration. Compare counters, IDs, canonical bytes and existing-row rewrite results. Fast/twin **online** list agreement and I-U/I-X/I-P do not catch a counter missing from persistence. Scratch evidence: `counter_probe.cpp`, `counter-v24.log` under `/work/scratch/ev2-rev-gpt/`.

### F3 — major: fixed FxHasher on raw pre-images admits cheap, exact service-key collision families

**Locations:** `designs/EV2-dml.md:121`, `:138`, `:779`; performance assumptions at `:765` and `:789-791`.

Equality of the pre-image keys preserves Core's collision classes under the stated SHA256d assumption. It does **not** preserve the computational behavior of Core's cryptographically hashed map keys. The fixed FxHasher choice is exposed directly to controllable service bytes.

Using `imbl = 7.0.2`, `rustc-hash = 2.1.3`, and the proposed `SmallVec<[u8; 48]>` key hashing, I generated 512 distinct 18-byte IPv6 CService pre-images. Fixing bytes 2..10 to the little-endian Fx byte-hasher seed `0x13198a2e03707344` collapses one mixer input to zero; varying bytes 10..16 preserves the collision. All use the global IPv6 prefix `2600` and port 19999. Results:

```text
control:   512 keys, 512 Fx hashes, 512 Core SHA256d hashes,    512 equality checks for all gets
collision: 512 keys,   1 Fx hash,   512 Core SHA256d hashes, 131328 equality checks for all gets
```

The sample `[2600:4473:7003:2e8a:1913::]:19999` passed Core's actual ExtNetInfo AddEntry and Validate in the C++ probe. This family applies to V24 extended-address lists; legacy MnNetInfo's IPv4 restriction does not admit it. These are ordinary distinct keys, not a cryptographic hash collision or one of quirk 23's equal-pre-image classes.

A collision bucket makes probes linear and insertion/removal workloads potentially quadratic. Refcounts, the invariant checker, and canonical comparisons remain correct, so they do not detect the performance failure. The design does not pin the FxHasher implementation; an unspecified implementation is insufficient protection against this demonstrated choice. This finding concerns the new Rust representation, not a Core vulnerability.

**Fix:** keep pre-image equality if desired, but use a specified collision-resistant table hasher or the deterministic OrdMap alternative with a bounded comparison cost. Add chosen-key workloads to PF0 and justify that choice against both normal and adversarial inputs. Do not infer that replacing SHA256d with FxHasher can only improve performance.

**Verification:** retain the collision fixture and assert lookup/mutation scaling and correctness. Scratch evidence: `/work/scratch/ev2-rev-gpt/fx-probe/src/main.rs`, `fx-probe.log`; the probe uses `GenericHashMap` to supply the custom hasher required by imbl 7's actual API.

### F4 — major: the differential gate mixes raw-helper inputs with a post-EV1 equivalence claim

**Locations:** `designs/EV2-dml.md:684`, `:831`, `:835`, `:838`, `:869-872`.

EV2 deliberately implements one reachable ProUpServ ordering because EV1 rejects invalid types before the v24 builder runs. However, `diff_dml_apply` calls the rebuilding helper with generated malformed payloads and compares its verdict against both pins. A ProUpServ with an unknown proTxHash and `nType = 7` gives `bad-protx-hash` in the v23 rebuilding helper and `bad-protx-type` in the v24 rebuilding helper. The one Rust implementation cannot equal both on that input.

The block-level example at line 684 is also wrong: v24 calls `GetValidatedPayload` → `IsTriviallyValid` before looking up the MN (`specialtxman.cpp:1098-1120`, `:1327-1341`). `providertx.cpp:420-422` rejects type 7 there, so the v24 block gets `bad-protx-type`, even when the hash is unknown. The C++ probe confirmed that early rejection. The design's assertion that both profiles get `bad-protx-hash` is not supported by the check loop.

There are related domain gaps: Core's canonical reader does not enforce I-P or require registration/revival heights to be no later than list height, whereas the top-run payee proof assumes reachable states. Arbitrary canonical lists need a declared domain or a fallback scan. Also `oracle_mnlist_projected(bytes, n)` lacks the MN_RR/deployment context that Core's projection requires. Finally EV2a-3 explicitly permits vectors-only acceptance until O2a-EV lands, although the charter requires differential fuzzing for consensus code.

**Fix:** distinguish integrated check-loop/block comparisons from raw rebuilding-helper comparisons. For the latter, validate and constrain the input to the intersection of the declared reachable domains, and test intentional out-of-domain helper differences separately. Correct the reject-string example. Supply deployment context to payee/projection oracles and specify the synthetic-list constraints. Make the relevant O2a-EV differential targets a prerequisite of the consensus implementation merge, rather than an eventual follow-up.

**Verification:** require both profiles' integrated unknown-hash/type-7 cases, all activation flag combinations admitted by the selected network, and valid multi-block sequences that check indexes and payees after each step and after reconstruction. Add persisted continuation/ban/revive/removal sequences across V19 and MN_RR before the broad mainnet replay. Record which layer rejects an input so a raw-helper disagreement cannot be silently normalized away.

### F5 — minor: the default-list codec/hash contract contradicts the pre-DIP3 vector gate

**Locations:** `designs/EV2-dml.md:377`, `:392`, `:846`.

The document correctly says Core hashes the default height −1 list, but then applies `ValidateCanonicalMNInvariants` before every encode and requires decode/re-encode at the pre-DIP3 boundary. Core's **serializer/hash** permits the default list (`snapshot.h:237-247`, `snapshot.cpp:150-155`); its **reader** rejects negative height (`snapshot.h:256`). The snapshot invariant validator also rejects it (`snapshot.cpp:106`). The probe encoded 41 bytes and then got `negative canonical MN-list height` on decode.

**Fix:** separate the Core-compatible canonical hashing encoder from validation of persistent snapshot records. Make the pre-DIP3 fixture a hash/encoding-only comparison, and scope decode round trips to the canonical reader's accepted domain. Do not claim the reader enforces I-P; that is an additional EV2 reachability check.

### F6 — minor: the 5.3× result is reproducible, but the model does not establish the final gain or floor

**Locations:** `designs/EV2-dml.md:756`, `:772-790`.

I reran the existing model with 3500 MNs, 400 initial penalized MNs and 4000 blocks. Fast/twin/setup instruction totals were 5,924,519,038 / 30,599,301,515 / 132,175,762: about 1.448 M and 7.617 M per block after setup subtraction, or 5.26×. This supports the quoted **model** result.

The model uses an OrdMap for unique keys and only two secondary indexes. Its twin shares the hot/cold layout, has an unused confirmation count, and omits BuildDiff and serialization; the fast path counts structural diff items without encoding them. It does not implement the full target workload. Its average diff contained 368.5 items per block, so the introductory `k ≈ 10–20` is not the effective k when eager decay participates. Raw variable-length hashing can add costs, and F3 supplies a counterexample to “the ratio can only widen.” Cold CHAMP-order materialization and full SML/Merkle work are other costs outside the eager-decay floor.

**Fix:** keep the numbers labeled as a limited model, correct the changed-entry count, remove the monotonic-gain assertion, and measure the complete implementation and actual Core components with the proposed PF gates. Include cold order computation, adversarial property keys, reconstruction, allocations and cache RSS; distinguish a build-only lower bound from total DML/consumer work. DEC-50's decision to defer lazy decay remains reasonable.

## Surfaces checked without an additional blocker

- **Payee ties and payouts:** `pay_height` preserves Core's `else if`; memcmp over internal bytes is the correct tie-break. The queue contains valid unconfirmed MNs. Selecting from the previous list preserves the previous payout scripts even if this block updates, bans, removes or replaces the selected MN. Projection's protected remaining streak followed by a sorted suffix is equivalent on reachable lists; MN_RR stops weighting and continuation. The V20 payment/reward gate belongs to EV3 and does not belong in the DML queue.
- **PoSe and indexes:** confirmations precede decay; valid penalized MNs decay before transactions; banned penalties stay frozen during decay but can receive further punishment. QcTx punishment is interleaved with registrations, with the unweighted partly rebuilt total determining the cap. The decaying and unconfirmed definitions include the right ban/confirmation conditions; evo-streak intentionally includes banned MNs. Updating bookkeeping after collateral removal and then resetting streaks matches both tags. I-X and the independent scans should catch index-maintenance mistakes provided the twin does not reuse the indexed helpers.
- **Unique properties and hot/cold state:** pre-image equality, cross-type aliases, reference counts, failure ordering and rollback match the inspected primitives under the stated cryptographic assumption. Preserving the collateral equality check in the ordinary-transaction spend pass is necessary and correctly specified. Hot updates can safely reuse cold SML hash slots selected by validity; implementation tests must also verify new cold state starts with empty derived caches.
- **Gates and effects:** V19 and MN_RR use `active_after(prev)`; V24 takes SpecialTxRules; there is no missing V20 list-state gate. The accepted v23-regtest V24 exception remains explicit. ResetPlatformBan carries the block height, runs only through the non-just-check ProcessBlock path, and precedes the enforcement-hash rejection. Equal-height unbans are refused by Core; the meta store should retain that exact behavior through reorg/retry tests. Core v24's existing-row write checks occur before the reset, so CH7a must preserve that error/effect ordering when staging writes.
- **Crash/reorg structure:** first-DIP3 plus 576-block snapshots, retained empty diff rows, retained disconnected rows, ancestry-based resolution, B1 atomic evo persistence and coins-only recovery fit A1. Missing committed history fails closed. These mechanics are sound but do not repair F1/F2's missing information. Crash tests should compare reconstructed historical lists as well as the optional tip snapshot.

## Validation evidence and limits

Core tests and the primary C++/Cargo probes used `TMPDIR=/dev/shm/ev2-rev-gpt-tmp` with mode 0700. The directory was removed after completion. Scratch sources, binaries and logs remain on disk in `/work/scratch/ev2-rev-gpt/`.

- Vendored-immer history probe compiled with `g++ -std=c++20 -O2` at both exact tags: identical collision insertion/replacement/erase behavior.
- v24 `evo_snapshot_tests`: 31 cases, 103,752 assertions passed.
- v23 `evo_dip3_activation_tests/{field_bit_migration_validation,migration_logic_validation}`: 2 cases, 52 assertions passed.
- v24 migration tests, canonical diff serialization test, and `block_reward_reallocation_tests`: 5 cases, 150 assertions passed. The actual Boost suite is `evo_dip3_activation_tests`, despite the source filename containing `deterministicmns`.
- Core-library counter/codec/type/ExtNetInfo probe reproduced F2/F4/F5 and accepted the sample F3 service.
- Rust SmallVec/FxHasher/imbl probe reproduced F3; a second probe with equivalent Vec slice hashing gave the same result.
- Existing scratch benchmark independently reproduced approximately 5.3× in instruction counters.

EV2 is a design, so there is no EV2 implementation to build or differential-test yet. The passing Core tests establish reference behavior and test availability, not EV2 correctness. No dashd instance was started or modified, and no private security issue was investigated.

Verdict: **REWORK**.
