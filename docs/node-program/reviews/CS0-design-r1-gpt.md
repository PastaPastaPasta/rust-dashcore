# CS0 design review, round 1

**Verdict: REWORK.** The overall layering is sound, and most specified Core behavior is correct. Before implementation, bind cached identifiers to their release, complete the header-hash handoff, correct the error-conversion contract, and make the differential evidence fail closed. These are bounded changes to the public design; they do not require a different type stack.

Reviewed `designs/CS0-node-primitives.md` dated 2026-10-08, A1 §§1.3, 1.4, 2.4, 2.25 and Appendix A, PLAN's CS0 row and §7, DEC-24, and R4's public consensus/verification material. DEC-25 appeared in the decision log during this review; its additional CS1 field writers are noted below. No private companion document, corpus, or excluded implementation module was read. This is a design review, not an implementation approval or a claim that the proposed tests have passed.

Core references below are to `v24.0.0-rc.3` unless otherwise stated. The inspected pins resolve to:

| Reference | Commit |
|---|---|
| v23.1.8 | `79b6c5215f33bd305ddf095747b932f516b7a9a7` |
| v24.0.0-rc.3 | `da567d7dba945143eee4d2ce1621c21b32089989` |
| develop | `15db47875002cc09d8a21e1e35fed4a74aaf4e4f` |

The inspected primitive, merkle, result-enum and context-free transaction-check files have no diff between the v24 pin and this develop revision. Acceptance evidence must nevertheless name the pin and Rust/base-sdk revisions it exercised.

## Findings

### B1 — BLOCKER — Release-dependent cached hashes have no enforceable provenance

**Sections:** §5, particularly lines 183–212; §6.3, lines 294–310; §8.4.

**Problem:** `Tx::new(raw, rules)` computes a release-dependent txid, then discards the rules identity. `TxRef` is unqualified, `Block::new(header, txs)` takes no rules, and neither wrapper exposes a release discriminator. A block can contain transactions constructed under different profiles, or a uniformly v24-constructed block can be passed to a v23 consensus context. There is no public check that can detect this without rebuilding the transactions.

For the public AU-v2 case specified in §5, take the same type-9 transaction with a 145-byte payload and payload version 2. Its cached v23 leaf is the full serialization hash; its cached v24 leaf has the last 132 serialization bytes zeroed. Pass a v24 `TxRef` into `Block::new` and then validate the result under v23 rules: the block merkle cache still uses the v24 leaf. All serialization bytes can be correct while the cached commitment is wrong for the consumer. A1 §2.25 row 5 explicitly requires different merkle/rejection behavior for this input. Core's `BlockMerkleRoot` uses `GetHash()` from transactions constructed by the same release (`consensus/merkle.cpp:65–72`); Core does not have this cross-profile object state.

The sentence requiring every cache to use its network's rules identifies the obligation but provides no way to enforce it. This matters for the proposed dual-pin conformance tests, selectable regtest profiles, miners, and future caches keyed by `TxHash`.

**Fix:** Retain a small hash-rule/release discriminator in `Tx` and `Block`. Make construction from `TxRef`s check their compatibility with the requested rules, and define the check at consensus-context/cache admission boundaries. A simple runtime discriminator and checked constructor are sufficient; generic profile parameters throughout the node would be unnecessary. Rewrapping raw transactions is the explicit way to change profiles. Specify tests that mix profiles and that use one profile's block with the other profile's context; the mismatch must be detected before cached merkle leaves or txids are trusted. This is an object-consistency check, not an additional wire-decoding restriction.

### M1 — MAJOR — The header API loses the binding and handoff needed to hash once

**Sections:** §4.4; §6.1, line 256; §6.3, line 306; §8.4; §11 CH1 consumer.

**Problem:** Blocks bind an immutable header to its cached X11 hash, but standalone headers return a freely separable `(BlockHeader, BlockHash)`. The header is `Copy`, and the A1 CH1 command carries `Vec<BlockHeader>`; A1's `check_block_header` signature also accepts only the raw header and parameters. Following these published interfaces drops the decoder's hash or recomputes it. Alternatively, accepting an arbitrary separately supplied hash permits pairing it with a changed header. This is both a hash-binding gap and a predictable CH1/CS3 interface change.

Core's header hash is X11 over the header's own 80-byte serialization (`primitives/block.cpp:13–18`). Its `CheckBlockHeader` takes the associated hash, and `CheckBlock` supports a known hash (`validation.cpp:4307, 4366`). No additional X11 operation should be necessary between decode and acceptance.

**Fix:** Specify one immutable decoded-header value with private header/hash fields and read-only accessors, or an equally constrained equivalent. Carry it through the CH1 header command and CS3 header checks. Construction from parts must compute the hash itself; do not expose an unchecked public header/hash constructor. A compile-only CH1/CS3 consumer should demonstrate the complete handoff without a second X11 call. This is a small wrapper, not a new header codec.

### M2 — MAJOR — `into_block` documents two distinct Core mappings as one

**Sections:** §8.1, lines 366–374; §9.1 error tests.

**Problem:** The helper's comment calls preservation of `GetRejectReason()` and `GetDebugMessage()` the “ConnectBlock / CheckBlock pattern.” That is correct for the `CheckTxInputs` failure in `ConnectBlock` (`validation.cpp:2697–2702`), but incorrect for `CheckBlock`. The latter preserves the reason and replaces the debug message with:

```text
Transaction check failed (tx hash <Core-display-txid>) <original-debug>
```

See `validation.cpp:4405–4412`; `ContextualCheckBlock` does the same at `:4562–4568`. Even an empty tx debug message produces that prefix and a trailing space. A straight conversion loses observable log/debug parity. Testing only the generic `Display` join does not catch the error.

**Fix:** Document `into_block` as a reason/debug-preserving class conversion only, with a specific `ConnectBlock` example. Document and test the separate CheckBlock/contextual mapping, either at those later call sites or through a helper accepting the transaction identity. Preserve exact formatting, including the displayed hash and empty-debug case. Do not automatically add the prefix to all conversions.

The enum lists themselves are correct: tx values are 0–11, block values 0–10, and the two pins differ only in the validation header's copyright line. Split error types, `Cow<'static, str>` reasons, and explicit class conversion are appropriate. `bad-version(0x%08x)` and both formatted script-failure reasons genuinely require owned reasons. `Display` should match the invalid-state portion of Core `ToString()`; representing valid state or runtime `M_ERROR` is outside this error type.

### M3 — MAJOR — Recorded codec divergences and crashes lack a complete gate contract

**Sections:** §9.3, lines 515–522; §9.5; §14 definition of done.

**Problem:** A differential mismatch writes an artifact, logs metadata, and continues. There is no specified machine-readable failure status or final gate assertion, so successful process completion and 24 CPU-hours do not establish a clean run. The proptest twins must also fail on mismatch while keeping inputs out of their reports. Merely assigning directory inspection to O3 triage is insufficient for CS0's acceptance gate, especially when CS0 creates the minimal targets before O3 exists.

The design additionally routes private seeds into these targets but permits genuine panics to abort normally after explicitly acknowledging that libFuzzer prints crashing bytes. Suppressing only mismatch panics and decoder debug assertions does not protect a private seed that triggers another Rust panic, a Core exception-path bug, or a sanitizer crash. This is a process/output issue; it makes no assumption about the excluded module.

**Fix:** Define a run-scoped verdict record: pin, Rust/base-sdk revisions, target, CPU-hours, mismatch count, crash count, artifact-write failures, and resolved/unresolved status. A run with a mismatch cannot pass; after a fix, require a fresh clean budget. Test the failure path with an intentionally mismatching oracle result. Failure to configure or write the private sink must stop the run with a sanitized diagnostic.

Capture raw fuzzer/sanitizer stderr and crash artifacts privately from process launch; publish only approved metadata. A runner may continue gathering inputs, but its gate must return failure on any divergence or crash. Proptest smoke tests should return a sanitized failure after recording the input, without exposing it through a default assertion/debug formatter. Require the private-corpus replay pass/fail in the gate report as PLAN §7.4 specifies. State explicitly whether the 24 CPU-hour budget is per target **per pin**, and record each leg rather than a pooled number.

### M4 — MAJOR — The planned tests leave critical decode and mutation boundaries implicit

**Sections:** §6.1–6.4; §7, line 328; §9.1–9.3.

**Problem:** Historical blocks, script transaction vectors, random object round trips, and generic “odd counts, mutation” tests are useful but do not guarantee coverage of the decoder domain or the distinct trailing-byte APIs. In particular, the stated mutation definition, “two equal adjacent hashes,” omits two essential qualifications: Core compares disjoint sibling pairs at positions `0/1, 2/3, …`, and it checks them **before** adding the synthetic odd-tail duplicate (`consensus/merkle.cpp:48–55`). Comparing all overlapping adjacent pairs, all repeated leaves, or post-padding siblings changes the flag and therefore the rejection classification. The Core anchor indicates the intended port, but the public contract should not leave this distinction to interpretation.

**Fix:** Add a deterministic boundary table alongside fuzzing:

- CompactSize minimality, truncation, and `MAX_SIZE` boundaries for every relevant vector/string, including the block transaction count; oversized semantic values must still decode where Core decodes them.
- Signed output values such as `-1`, `i64::MIN`, `MAX_MONEY + 1`, and `i64::MAX`; empty vin/vout; unknown transaction types; payload lengths around the former decoder cap and Core's decoder cap. Rule failures remain CS3's responsibility.
- Every prefix truncation of representative fixtures; a complete tx/header/block followed by a suffix; two transactions decoded consecutively. Assert cursor movement and `consumed`, and assert that `decode_exact` rejects the tx suffix while stream tx/block/header decoding preserves it. Apply the RPC-vs-P2P policy explicitly in oracle comparisons; O1's inspected codec API is streaming, not `DecodeHexTx`.
- Stable-txid cases under both pins: payload lengths 144/145/146, payload versions 1/2/3, predicate-off cases, and modifications to the signing suffix versus the index/fee prefix. Compare Core re-serialization, txid, instance hash, and block leaves, not merely equality between two Rust constructors.
- Merkle cases with distinct symbolic leaves: empty and singleton; `[a,b,c]` is not mutated; `[a,b,b]` is not mutated; `[a,a,b]` is mutated; `[a,b,c,d,e,f]` and `[a,b,c,d,e,f,e,f]` have equal roots but different mutation flags. The last case detects mutation above the leaf level. Expected roots/flags come from O1.

Exercise both wire-hash and re-serialization-hash paths against independent Core expectations. Coverage of any non-public cases can be certified by the authorized reviewers and sanitized private-test results; this review requests no inputs or explanation of excluded behavior.

The synced-range merge policy is consistent with the design's stated milestone split, but its evidence must say **partial history** and record the actual networks, profiles, heights/stale-block coverage, and pinned revisions. It cannot be reported as having met PLAN's eventual full-history criterion.

### M5 — MAJOR — The private-test environment contradicts DEC-24

**Sections:** §9.4; §10 test run table; §13 D6; §14 G0.

**Problem:** The design still proposes a box-wide Cargo `[env]` and a test that fails when the variable is unset. DEC-24 D6 explicitly rejects that approach: ordinary task runs skip with a loud message; the manager's integration gate supplies the environment and guarantees the private tests run. The current design would fail ordinary runs without the configuration or expose private test execution to all worktrees after installing it. This is already decided and does not require another approval question.

**Fix:** Rewrite these sections to implement DEC-24. Unset environment in an ordinary run produces the prescribed loud skip. In the integration gate, assert the variable is set, the corpus is non-empty, and the named tests actually executed and passed; a successful Cargo exit containing a skip is insufficient. Set the environment only for the gate's test processes. Keep the designated-reader and private-output restrictions. Mark all §13 decisions resolved according to DEC-24 rather than leaving them pending.

### M6 — MAJOR — The benchmark comparison cannot establish the proposed performance result

**Sections:** §6.3, line 309; §7, lines 333–335; §9.6; §15.

**Problem:** `block_decode_hash` includes transaction hashes, X11, allocation/destruction, and merkle construction. Core `DeserializeBlockTest` constructs the transactions and their cached txids but does **not** compute the block X11 hash or merkle root (`src/bench/checkblock.cpp`). Therefore the Rust workload is not merely deserialize plus merkle as stated. `DeserializeAndCheckBlockTest` adds other checks too. Reporting these three names together does not provide a matching denominator for parity or a 2× claim.

The SHA assessment also needs correction. The installed `bitcoin_hashes` 1.2.0 `hash_64_many` already dispatches to AVX2 eight-way and SSE4.1 four-way implementations with `std` or `cpufeatures`; absence of SHA-NI does not make that merkle operation scalar. Conversely, its ordinary streaming SHA256 engine falls back to scalar code without SHA-NI on this x86 host, whereas Core has an SSE4 single-stream backend. Feature selection and single-stream hashing, rather than an assumed missing AVX2 merkle implementation, are the immediate questions. The safe `hash_64_many` API has separate input/output slices, so its packing/copying cost also belongs in the merkle measurement.

**Fix:** Add a matching Core workload that deserializes, obtains the header X11 hash, and computes `BlockMerkleRoot` with the mutation flag, with the same allocation/destruction boundary as Rust. Keep the existing Core rows as diagnostic comparisons and keep the 9,001-leaf workload separately. Record the enabled Rust features, detected Core backend, pin, rules, compiler/profile, and whether input preparation and pair packing are timed. Enable the intended existing hash backend explicitly before proposing a new port. Run comparisons in a manager-reserved quiet window per DEC-23, not simply “the same window.”

Correct the merkle cost statement: `n−1` pair hashes applies to power-of-two leaf counts, not all counts; three leaves require three pair hashes, and five require six. Reuse bounded scratch storage rather than allocating a full pair vector at every level if measurement shows packing matters.

**2× assessment:** Decode once, reuse ranges, cache hashes, and lazy payloads are sensible. But Core already caches ordinary transaction hashes and deserializes outer transactions without typed payload parsing. The gain over Core here principally removes its transaction re-serialization work; it does not remove the cryptographic work. An eager Rust merkle and X11 calculation adds work to a decode-only comparison. On this host, a slower single-stream SHA backend can consume the serialization savings. A 2× primitive-workload result is unproven, and CS0 alone provides no defensible 2× end-to-end validation claim. The wider A1 pipeline may achieve its hypothesis through measured savings elsewhere; retain that distinction and report a shortfall honestly.

## Smaller clarifications and scope assessment

- **MINOR — §8.4 ancestry contract:** `ancestor` returns a total `BlockId`, whereas Core `GetAncestor` returns null for a height above the starting block (`chain.cpp:94–98`). Either specify and enforce the precondition `height <= header(id).height`, or return `Option<BlockId>` for that case. Specify valid-id and arena-lifetime requirements. This avoids consumers inventing different failure behavior without requiring a generational arena redesign.
- **MINOR — §4.3 CS1 granularity:** Incorporate DEC-25's `write_prevout` and `write_sequence`; make `write_input` compose them. The accepted additions address the sighash consumer's need to serialize individual fields. The broad serializer ownership, signed `write_output`, and sole-encoder direction remain appropriate.
- **MINOR — §6 failure cursors:** State whether a failed decode leaves the cursor partially advanced and what `DecodeError.offset` is relative to. Block decode should add the header/count/preceding-transaction offsets and `tx_index` consistently. No rollback guarantee is needed unless a caller requires one.
- **MINOR — §8.4 contexts:** Document that checkpoint disablement is represented by no effective checkpoint, and that height/prev, MTP, deployments, BLS scheme, and rules refer to the same chain/profile. `adjusted_time` is actually the profile-selected comparison time in v24; a neutral name such as `header_check_time` would reduce misuse. This is a naming/contract issue, not a request to add a clock to the pure crate.

The rest of the public design is appropriately restrained. Immutable wrappers and Arc sharing prevent stale hashes after transaction edits. Central field writers, hash/size sinks, raw payload retention, per-scheme lazy payload caches, split validation result types, and a small backend seam have concrete consumers. There is no need for a pluggable codec framework, generic release parameters on every type, global BLS state, or a full runtime `ReleaseProfile` in the pure crate. Leaving the private sigop-cache field to CS3 does not force a public API change.

The fixed-width header order and X11 choice match Core. The stable-txid predicate and 132-byte zeroing match v24 and are absent in v23; the rule is release-dependent, not deployment-dependent. The exact-one-null-input coinbase predicate is correct. Streaming tx/block/header decode and exact RPC transaction decode reflect `core_read.cpp:105–159`. Deferring output/payload semantic limits to rules and waiting for BS4a before merging is correct. `DeploymentView`'s optional previous block and `Sync` bounds, the runtime aggregate split approved by DEC-24, and the common `BlockId`/ancestry interfaces are adequate for EV0/CS4 once the contracts above are explicit. Scheme-aware payload implementation remains owned by the designated BS tasks; the public signature alone is not evidence of BLS parity.

After B1 and M1–M6 are addressed in the design, the proposed L0/L1, BS2, private gate, and matched benchmark evidence form a credible CS0 acceptance plan. Approval of the excluded module and its private cases remains with the authorized reviewers.
