# CS0 design review, round 2 (focused)

**Verdict: REWORK. No blocker; two major findings below.** The original release-provenance blocker is resolved by the alternative approved in DEC-30. Most round-1 findings are resolved. The remaining changes are bounded: make the new `CoreBytes` guarantee enforceable and reject an empty private acceptance corpus.

Reviewed `designs/CS0-node-primitives.md`, revision r1 dated 2026-10-08, against `reviews/CS0-design-r1-gpt.md`, its §16 dispositions, and DEC-24, DEC-25 and DEC-30. This is a public design review. No private companion note, corpus, or excluded module was read; their absence from this review is expected. Approval of those parts remains with the designated reviewers. No implementation or test-run approval is implied.

For the focused source checks, inspected Dash Core `v24.0.0-rc.3`, peeled commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`: `src/primitives/transaction.cpp`, `src/consensus/merkle.cpp`, `src/validation.cpp`, and `src/bench/checkblock.cpp`. The round-1 hash `da567d7dba945143eee4d2ce1621c21b32089989` is the annotated tag object for that commit, not a different source revision.

## Round-1 findings: verification

| Finding | Round-2 assessment | Evidence and remaining qualification |
|---|---|---|
| B1 — release provenance | **Resolved** | §§5–5.1 and §6.3 cache both deterministic hash alternatives without a selected release. `Tx::txid(rules)` selects the appropriate hash; `Block::merkle(rules)` selects the appropriate **root and mutation flag together**. Construction and decoding take no rules. §9.1 checks the same objects under both pins and blocks assembled from separately decoded references. §11 binds runtime txid-keyed caches to their network's rules. DEC-30 N1 approves this approach. |
| M1 — header binding and handoff | **Resolved** | `Header` has private raw/hash fields, only a hash-computing public constructor, and read-only accessors (§6.3). CH1 carries `Vec<Header>` and CS3 checks take `&Header` (§11, deviation 7). The 2,000-header handoff test counts X11 calls. Copying the pair does not weaken its binding. |
| M2 — error conversion | **Resolved** | §8.1 distinguishes reason/debug-preserving `into_block` from `into_block_wrapped` for CheckBlock/ContextualCheckBlock. The latter takes the rules-selected txid, uses Core's reversed-byte hex display, and retains the trailing space for empty debug. Both mappings and the hash display have independent expected-string tests. This matches the inspected Core call sites. |
| M3 — differential failure and output gate | **Resolved at design level** | §9.3 captures fuzzer stdout/stderr from launch, confines child logs, corpus writes and artifacts, specifies run-scoped provenance and verdicts, fails on divergence/crash/sink failure or insufficient CPU time, and requires a fresh budget after a fix. Shared comparisons prevent target/twin drift. Twins suppress input formatting and persistence. The runner self-tests mismatch and sink failure. Four separate 24 CPU-hour legs and two separate one-hour header legs are explicit. Private replay is part of §9.4's gate, subject to R2-M2 below. |
| M4 — decode and mutation boundaries | **Resolved** | §9.1 enumerates CompactSize minimality and limits across relevant fields, signed/oversized output values, transaction shapes/types, payload caps, prefix truncations, suffixes, consecutive decodes, cursor rollback and consumed ranges. Stable-txid predicates and suffix/prefix changes are checked on both pins against serialization, both hashes and block roots. Both hashing paths have independent expectations. §7 specifies disjoint sibling pairs before odd-tail padding on **every level**, and the table includes mutation above the leaf level. §9.2 labels merge evidence partial history and records actual coverage. |
| M5 — DEC-24 private-test environment | **Partially resolved** | §9.4 removes box-wide configuration, provides a visible ordinary-run skip, uses gate-local environment, and checks fresh count receipts rather than Cargo status alone. Missing/altered/extra cases are rejected. However, the round-1 requirement for a **non-empty** corpus is still absent; see R2-M2. |
| M6 — benchmark comparability | **Main finding resolved; minor qualification remains** | §9.6 adds `DeserializeBlockHashMerkle`, including decode, X11, merkle mutation, and per-iteration construction/destruction; DEC-30 N4 approves ownership. Backend/features, revisions, rules, compiler/profile and timed preparation are recorded; comparisons require a reserved quiet window. §7 corrects pair-hash counts and reuses two buffers without pair packing. The existing multi-way backend versus missing single-stream SSE4 distinction is corrected. No 2× claim remains. The new transaction-only diagnostic row is not exactly matched; see R2-m1. |
| Minor — ancestry | **Resolved** | §8.4 returns `Option<BlockId>` above the starting height, specifies valid arena IDs/lifetimes, and permits by-value `HeaderCtx`. |
| Minor — DEC-25 writers | **Resolved** | §4.3 adds `write_prevout`, `write_sequence`, and their composition in `write_input`. |
| Minor — failure cursors | **Resolved** | §6.1 explicitly guarantees no cursor movement on failure and defines offsets relative to the call's input, including the block transaction index. An item-start offset is less granular than an inner-byte offset but is now an unambiguous contract. |
| Minor — context coherence | **Resolved** | §8.4 specifies disabled checkpoints as `None`, one chain/profile per context, and `header_check_time` selected by the profile. |

The exact-comparison change also addresses the intended serialization concern: `CompareWriter::matches()` requires both byte equality and final length equality, comparison uses the consumed range, size comes from serialized output, and the stable-hash scratch copy uses the selected canonical bytes (§§4.2, 6.2). Block canonicality checks the header/count plus every transaction. This is sound internally; the new public proof-bearing wrapper must preserve that result, as described next.

## Remaining and new findings

### R2-M1 — MAJOR — `CoreBytes` can be forged through its public APIs

**Sections:** §4.2 lines 144–156; §4.5 lines 209–220; §6.1 lines 298–305.

The new type is supposed to make canonical storage/relay/RPC bytes a compile-time property across crates, replacing the rejected node-wide encoder lint. Its tuple field is private, but both public factories admit unverified bytes.

First, all three fields of `Decoded` are public and mutable. A consumer can obtain a real decode and then change its proof-bearing fields:

```rust
let mut decoded = decode_tx_exact(valid_tx_bytes)?;
decoded.wire = b"arbitrary bytes";
decoded.wire_is_canonical = true;
let bytes: CoreBytes = decoded.core_bytes();
```

Under the specified method contract, this wraps `arbitrary bytes` as canonical. Replacing `decoded.value` while retaining its original wire/flag creates the same problem. Private fields on `Tx` and `Header` do not protect the association in `Decoded`.

Second, `core_bytes<T: CoreSerialize>` accepts a downstream type implementing the public, unsealed trait. Such an implementation can emit arbitrary bytes, including output obtained from a base-sdk encoder. It then acquires `CoreBytes` without passing any CS0 canonical encoder. This is an ordinary safe API use, not a local-toolchain tampering scenario.

**Required change:** make the fields forming `Decoded`'s value/wire/canonicality association private, expose read-only accessors, and provide a consuming `into_value` for extraction. Do not expose a setter or unchecked public constructor that preserves the proof after replacement. Restrict **both** `CoreBytes` factories to a sealed set of supported consensus object types, either by sealing `CoreSerialize` or by using a separate sealed bound for these factories. An open serialization trait can remain available for other writers if needed. `CoreBytes` construction must ultimately use CS0's encoder or an internally established wire-equality proof. Add compile-fail/API coverage showing that downstream consumers cannot replace the association or mint `CoreBytes` through a custom serializer.

DEC-30 approves the type approach; this finding concerns the public API's failure to enforce that approved invariant.

### R2-M2 — MAJOR — The private gate can pass with an empty corpus

**Sections:** §9.4 lines 647–649; §14 lines 785–791; §16's M5 disposition.

Round 1 explicitly required the integration gate to assert a non-empty corpus. The revised contract rejects a missing manifest and inconsistent files/counts, but never rejects an empty manifest. With a syntactically valid zero-entry manifest and no case files, the driver can check zero cases, emit a fresh receipt containing zero, and satisfy the gate's `receipt_count == manifest_count` check. Thus all specified checks pass while `quirk_01_embargo_1` has exercised no input. The same shape permits a zero-seed private replay unless its required coverage is checked separately.

This does not claim anything about the actual private corpus. It identifies a missing public gate requirement that must catch accidental empty provisioning.

**Required change:** explicitly reject a zero-case quirk manifest and require the manager's gate to verify a positive executed count as well as freshness and manifest equality. Require the private replay to execute the non-empty seed coverage mandated for the current gate, according to its private manifest/checklist; retain aggregate-only public output. A public synthetic empty-manifest self-test should demonstrate a sanitized failure rather than a successful zero-count receipt. This completes round-1 M5 without exposing private case content or target identities.

### R2-m1 — MINOR — The transaction-only benchmark row is not an exact match

**Section:** §9.6 lines 661–669.

`block_txs_decode` decodes transactions only, whereas Core's `DeserializeBlockTest` also decodes the header and transaction count, allocates/populates the block's vector of shared transaction references, retains those references until block destruction, and rewinds the stream within each timed iteration (`src/bench/checkblock.cpp:21–33`). The Rust row does not specify the same collection/allocation lifetime and expressly omits the header. Calling these workloads matching is inaccurate even though neither computes X11 or merkle.

Either label that pair diagnostic and record the differences, or provide a benchmark-only Rust workload with equivalent header/count decoding, collection and destruction boundaries. The approved full `DeserializeBlockHashMerkle` comparison remains suitable; this minor correction need not redesign the production decoder.

### R2-m2 — MINOR — §13 still labels approved decisions as pending

DEC-30 has already approved N1–N6. Change §13's “Needed from the manager” status to reference those approvals, including CS0's ownership of the local Core benchmark patch. No further approval request is needed for those choices.

## Acceptance after revision

The alternative release-provenance design is accepted. The immutable header, exact comparison, separate error mappings, merkle mutation definition, deterministic decode boundaries, contained fail-closed fuzz runner, and matched full-block benchmark form a credible implementation plan. Resolve R2-M1 and R2-M2 in the public contract before implementation approval; correct the two minor statements when updating the document. No access to the excluded module is required to resolve these findings.
