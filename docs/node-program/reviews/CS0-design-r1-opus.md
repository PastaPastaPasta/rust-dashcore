# CS0 design review, round 1 (Opus)

- **Reviewed:** `designs/CS0-node-primitives.md` (draft of 2026-10-08) and its private companion note (PLAN §7.2).
- **Context:** A1 §1.3, §1.4, §2.4, §2.25 and Appendix A; PLAN row CS0 and §7; DEC-24 and DEC-25; R4.
- **Role:** one of the two designated EMBARGO-1 reviewers (PLAN §7.2 item 3). EMBARGO-1: see ~/private/embargo/EMBARGO-1.md. Findings that concern EMBARGO-1 are in the private review, `~/private/embargo/CS0-design-review-opus.md`. This file gives them only by ID and says "see private review".
- **Scope:** I wrote only this file and the private review. My one experiment (libtest output capture) ran in `/work/scratch/cs0-review-opus/`.

**Summary:**
- The marked module's design reproduces Core's behaviour for EMBARGO-1 on both pins, and the fast path never hashes wire bytes that differ from Core's re-serialization. I checked this against both tags line by line (see private review §1).
- Containment has one blocker and several gaps:
  - the tracked shape of `quirk_01_embargo_1` (B-1);
  - DEC-24 D6 is not reflected yet (M-1);
  - fuzz corpora, crash artefacts and logs of the codec targets are uncontrolled (M-2).
- The rest of the design is sound and lean. The decode domain, hashing, merkle, `is_coinbase`, the release-dependent txid and `ValidationError` all follow Core. These need fixing:
  - a round-trip property that is false as stated (M-4a);
  - a stable-txid vector source that does not exist (M-4b);
  - a `TxType` equality pitfall (M-5);
  - the sole-encoder rule is enforced only inside `dash-consensus` (M-6).

**Verdict: APPROVE-WITH-CHANGES.** B-1, M-1 to M-6 and the private majors are changes to the design documents only. They should be applied before implementation starts. A short round 2 on the changed sections is enough.

---

## What I checked

| Check | Result |
|---|---|
| Core `v23.1.8` and `v24.0.0-rc.3`: transaction and block serialization and hashing (`primitives/transaction.{h,cpp}`, `primitives/block.{h,cpp}`), `consensus/merkle.cpp`, `consensus/validation.h`, `core_read.cpp`, `consensus/params.h`, `deploymentstatus.h`, `evo/cbtx.cpp`, and the tx-to-block state conversions in `validation.cpp` | the rules the design ports are identical on both pins, except the stable txid and instance hash (v24 only, as designed) and the `DeploymentPos` superset. Details per finding below and in the private review |
| Stable txid constants | `ASSET_UNLOCK_QUORUM_INFO_SIZE` = 4+32+96 = 132 and `ASSET_UNLOCK_PAYLOAD_SIZE` = 145 (`transaction.h:47,50`); stable version 2 (`:53`). `GetInstanceHash` and `ComputeStableUnlockHash` are at `transaction.cpp:70-125`. Merkle leaves are `GetHash()` on both pins (`merkle.cpp:65-73`). The asset-unlock root uses instance hashes without a mutation check (`cbtx.cpp:158-172`) |
| Core vector files | `block813851.raw`, `tx_valid.json`, `tx_invalid.json`, `sighash.json` and `trivially_{valid,invalid}.json` exist on both tags. **`trivially_valid.json` has no asset unlocks on either tag**: 25 rows, all ProTx types 1–4 (M-4b) |
| base-sdk `33fcc59` | `CompactSize::decode` rejects non-minimal encodings (`types/src/compact.rs:51-83`). `Vec<T>::decode` bounds the count by the remaining bytes and allocates in batches (`codec.rs:307-321`). `BlockHeader` fields round-trip. `TxType` is an `enum_map` with an `Unknown(u16)` catch-all and a derived `PartialEq` (M-5). `is_coinbase` is `pub(crate)` and accepts any input count. `Transaction` derives `Debug` |
| O1 shim (`oracle-shim-v24.0`, `oracle.cpp:458-509`) | `oracle_tx_decode`, `oracle_block_decode` and `oracle_header_decode` stream-decode and report `consumed`, the re-encoding and the hashes; instance hash = txid below v24. The design's §9.3 comparisons are all available |
| `bitcoin_hashes` 1.2.0 | `sha256d::Hash::hash_64_many` exists (`sha256d/mod.rs:20`). With MSRV 1.89, `as_flattened`/`as_chunks` give the 64-byte pairs without `unsafe` |
| libtest capture (scratch crate, `cargo test`) | a passing test's `eprintln!` is **captured and not shown**; `std::io::stderr().write_all` is shown (M-1) |

---

## Blocker

### B-1. The tracked shape of `quirk_01_embargo_1` (containment; see private review P-B1)

As specified, the tracked test would have to name the private vectors' files, fields and later-added assertion targets. PLAN §7.1 forbids that in tracked text. **Required:**
- the tracked test is a schema-opaque driver whose code names only what the public `diff_tx_decode`/`diff_block_decode` targets already compare;
- everything else stays in private data;
- D7's later assertions are replaced by a mechanism that needs no tracked change at CS1's or CS5b's merge.

The private review gives the exact schema and the replacement for D7. Tracked §9.4, §10 and §13 D7 change accordingly.

---

## Majors

### M-1. DEC-24 D6 is not reflected, and "skip with a loud message" needs a precise mechanism

Tracked §9.4 and §13 D6 still say the test fails when `DASH_EMBARGO_DIR` is unset, with a box-wide `[env]`. DEC-24 decided the opposite. Rewrite §9.4:
- **Unset:** the test passes after printing one codename-only line. libtest captures `eprintln!` from passing tests (verified), so the line must use `std::io::stderr().write_all(…)` to be seen at all.
- **Set:** the test **fails** if `quirk1/` is missing or empty, or if it checks fewer cases than a `MANIFEST` in the corpus declares. This makes a partial or stale corpus visible.
- **Positive evidence for the gate.** PLAN §7.5 ("never skip silently") now rests entirely on the gate script's environment. If a wrapper drops the variable, the test passes silently. The test should emit a run receipt with a count only: a fixed stderr line, or a receipt file inside the private tree. The gate asserts the receipt is present and that the count matches the `MANIFEST`.
- **No tracked file sets the variable:** no repository `.cargo/config.toml`, `Justfile`, CI group or tracked gate script. Only the manager's gate does.
- **Fuzz targets** whose divergences go to the private sink refuse to start when the variable is unset. They never fall back to stderr or a panic (M-2).

### M-2. Fuzz and property-test output: control the corpus, crash artefacts and logs, not only divergences (see also private review P-M5)

§9.3 routes divergences well. Three other channels remain, and none goes through the sink:
- libFuzzer prints crash inputs of up to 256 bytes in Base64.
- It writes `crash-*` files to the artifact prefix, by default the worktree's fuzz directory.
- It saves every coverage-increasing input to the corpus directory.

**Required for the three codec targets:**
- the working corpus, `-artifact_prefix` and stdout/stderr all go under `$DASH_EMBARGO_DIR/fuzz/<target>/` (`corpus/`, `crashes/`, `logs/`);
- `node-fuzz` ignores artefacts in `.gitignore`;
- only aggregate statistics leave the private tree;
- private file names are opaque (sha256 or `case-NNN`), because libFuzzer prints paths (`Running: …`, `Test unit written to …`).

**Proptest twins:**
- the generated seed goes in a newtype whose `Debug` prints a fixed placeholder;
- `failure_persistence: None`;
- a mismatch goes to the sink (when the variable is set) and panics with target, field and sink id only.

Otherwise a failing twin prints a seed that reproduces the input for anyone.

**One sink for all.** Ship the sink as a shared helper (`node-fuzz` or `dash-conformance`) and make it the default for every L1 target whose input contains transaction or block bytes, which is most of them. Then no target needs its own output policy.

**Output discipline, made mechanical.** In the quirk test, the private-seed loaders, the sink and the twins:
- `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::use_debug, clippy::dbg_macro, clippy::print_stdout)]`;
- no `assert_eq!`/`assert_ne!`/`debug_assert_eq!` anywhere operands could come from private inputs: they print both operands' `Debug`, and `Transaction` derives `Debug`. The decoder's assertion must be `debug_assert!(a == b, "<static text>")`;
- a review grep for those macros.

**Modes.** The sink sets 0600/0700 explicitly. agentbox's umask is 002.

### M-3. Make the fast-path predicate exact; preferably a streaming comparison (see also private review P-M1)

§4.3's contract is one-directional: the predicate is "true **only when**" the bytes are equal. Two places state it as an equivalence:
- `Decoded::wire_is_canonical`'s doc comment ("the consumed bytes equal `core_serialize(value)`");
- §9.2's acceptance check ("`wire_is_canonical` agrees with a byte comparison").

As written, the BS2 leg can fail on real history. **Preferred fix:** decide the fast path with an allocation-free `CoreWrite` sink that compares the bytes `core_write` produces against the wire range, instead of a field-level predicate. It is exact by construction, needs no sufficiency argument, and does not depend on base-sdk's decode domain for its soundness. It also improves containment (private review). Its cost is one pass at about memcmp speed beside two software SHA-256 passes; measure it in `block_decode_hash`. §9.2 can then assert that **every stored transaction and block is `wire_is_canonical`**, a free, strong check that Core stores its own serialization.

### M-4. Verification-plan errors

- **(a) The round-trip property is false as stated.** "decode ∘ `core_serialize` is the identity on generated transactions" fails for generated values that are not in decoded form. One example: a `TxType::Unknown(n)` that shadows a named variant decodes back to the named variant. State it as:
  - **idempotence:** `core_serialize(decode(core_serialize(x))) == core_serialize(x)`;
  - **identity only where `wire_is_canonical(x)`;**
  - generators build `TxType` through `TxType::from_base`.
- **(b) The stable-txid vectors have no source.** §9.1 cites "`trivially_valid.json` asset unlocks", but that file holds only ProTx types 1–4 on both tags. Generate the cases with O1's `oracle_tx_decode` on **both** pins:
  - payload length 144, 145 and 146;
  - `payload[0]` 1, 2 and 255;
  - versions 2, 3 and 4;
  - each under `UngatedRules::V23_1_8` and `V24_0`.

  Port the cases of Core's `src/test/evo_assetlocks_tests.cpp:525-545` (txid vs instance hash, re-signed instances). Add all of them to `diff_tx_decode`'s seeds, so the release-dependent txid is fuzzed on both pins.

### M-5. `TxType` equality is not Core's `nType` comparison

base-sdk's `TxType` has an `Unknown(u16)` catch-all and a derived `PartialEq`, so `TxType::Unknown(0) != TxType::Spend` although both are `nType == 0` (`enum_map`'s own test shows the shadowing). base-sdk's `has_extra_payload` and `Checkable` use enum equality. Decoded values are safe, because `from_base` canonicalises them. `Tx::new` and every caller that builds a `Transaction` directly (the miner, tests, CS1) accept any value, however. A constructed `version 3, Unknown(0)` transaction would then get a payload field Core would not write, and so a different txid.

**Required:**
- every predicate and writer in `canonical`, and the stable-txid rule, compares `tx_type.to_base()` numerically;
- `Tx::new` canonicalises `tx_type` (`TxType::new(to_base())`) or rejects a non-canonical one;
- the same rule goes into CS3's brief for every type test.

### M-6. The sole-encoder rule is enforced only inside `dash-consensus`

The `clippy.toml` ban on `BaseCodec::encode` and `Hashable::hash` covers this crate only. Runtime crates will hold base-sdk `Transaction` and `Block` values, and base-sdk's encoders, its `Hashable` impls and its P2P message types (which embed those values) are one call away:
- ST2 block files;
- P\* outbound `tx`, `block`, `cmpctblock` and `blocktxn`;
- RPC hex output.

A1 §1.3 makes `core_serialize` the only encoder for consensus objects. CS0 is where that rule is defined, so it should also ship its enforcement:
- the lint template for every node crate (at least `Hashable::hash`);
- an interface rule in `ARCHITECTURE.md` and the P\*/ST2/RP briefs: outbound and stored transaction and block bytes come from `core_serialize`, or from received bytes when `Decoded::wire_is_canonical` holds, never from base-sdk encoders;
- preferably, outbound message variants for those commands that carry pre-serialized bytes.

### Private majors (EMBARGO-1; see private review)

P-M1 to P-M6:
- the fast-path predicate (with M-3);
- the coverage of the private vectors and the private L3 operator;
- two of the brief lines the private note proposes for later tasks;
- the definition of scan (g);
- fuzz-corpus details (with M-2);
- where private regtest runs keep their datadirs and logs.

---

## Minors

- **m-1. `TxValidationError::into_block` covers only `ConnectBlock`.** `CheckBlock` (`validation.cpp:4411`) and `ContextualCheckBlock` (`:4567`) wrap the debug message as `"Transaction check failed (tx hash %s) %s"`. Only `ConnectBlock` (`:2702`, `:2755`) passes it through unchanged. Fix §8.1's comment and give CS3 a way to set the wrapped debug message (`into_block(..).with_debug(..)` works if `with_debug` replaces the message). The hash in that message is `GetHash()`, the release's txid. With that change, D4's shape is fine as designed.
- **m-2. Define "mutated" exactly.** Core compares pairs at **even** offsets on each level, before duplicating an odd tail (`merkle.cpp:45-63`). "Some level has two equal adjacent hashes" over-matches. Test these cases:
  - `[A,B,B,C]` → not mutated;
  - `[A,B,C,C]` → mutated;
  - `[A,B,C]` (the odd tail is duplicated) → not mutated;
  - a mutation that appears only at level 2.
- **m-3. `dip0003_enforced` is not a view method.** Core's `DeploymentDIP0003Enforced(nHeight, params)` is a `constexpr` height comparison (`deploymentstatus.h:66`; v23.1.8 `:53`). Each call site picks its own height: `pindexPrev->nHeight`, `pindex->nHeight` or a quorum base. Make it a free function of `(height, params)` in CS4's module, ported verbatim, and record it as a deviation from A1 Appendix A.
- **m-4. `ChainAncestry` signatures.**
  - `ancestor` should return `Option<BlockId>`: Core's `GetAncestor` returns `nullptr` for heights above the block or below zero, and DGW and versionbits callers rely on that.
  - `header(&self, id) -> &HeaderCtx` may be impossible to implement over a locked or epoch-managed arena. Make `HeaderCtx: Copy` and return it by value.

  The EV0/CH1 owners should confirm both changes.
- **m-5. Fold in DEC-25 (2) and co-locate the predicates.** Add `write_prevout` and `write_sequence` to §4.3, with `write_input` as their composition. Put **every** Core predicate on `CTransaction` in one module, including `is_coinbase` and the asset-unlock predicates the stable txid uses, rather than splitting them between `canonical` and `Tx`. One place to port, review and test them.
- **m-6. Tracked-text hygiene (see private review P-m1).** Two passages of the tracked doc should move to the private note; the private review lists them. With B-1, §10's sentence on later tasks' assertions goes.
- **m-7. Fuzz budget per pin.** State "≥ 24 CPU-h per target **per pin**". The PLAN row is ambiguous now that every target runs on two pins.
- **m-8. Check A1's mainnet claim directly.** §9.2 compares mainnet history under `UngatedRules::V23_1_8` against the v24 oracle. That rests on A1 §2.25's argument that row 5 never fires on mainnet history. Run both rule sets over mainnet history in the BS2 leg and assert equal txids and merkle roots. It costs almost nothing and turns the argument into evidence.
- **m-9. One pin delta for O17 (`UngatedRules` candidate).** `ContextualCheckTransaction` accepts special types 0–9 in v23.1.8 and 0–12 in v24.0.0-rc.3. On mainnet, where V24 is never active, a type-10 transaction is rejected as `bad-txns-type` under v23.1.8. Under v24 it goes on to `CheckProDisTx` (`bad-prodis-too-early`, `specialtxman.cpp:1799`, after payload validation). Both releases reject it, with different strings and classes. This is not in A1 §2.25's table. R2 and R4-B describe only the v24 list.
- **m-10. `Tx::into_raw(self)` does not fit `TxRef`.** `Tx` normally lives in an `Arc`. Add `to_mutable(&self) -> Transaction`, which is Core's `CMutableTransaction(const CTransaction&)`.
- **m-11. The stable-txid scratch copy.** §6.2 says it copies `wire`. Say "the bytes being hashed (the wire range on the fast path, `core_serialize` otherwise)", so the code does not rely on the two being equal.
- **m-12. One comparison function for both fuzz forms.** Share it between each libFuzzer target and its proptest twin: `compare_tx(bytes, pin) -> Result<(), Divergence>` in `dash-conformance`, called by both, with the sink behind it. Two comparison implementations will drift.

## Notes (no change needed)

- **Decode domain (§6.4).** Inheriting base-sdk's domain with a per-DDE-row allowlist that is gone at merge is right. CS0's own block decoder adds only the `MAX_SIZE` count bound and batched reservation, both matching Core's vector deserialization. Zero-transaction blocks decode, as in Core.
- **Trailing bytes.** `decode` vs `decode_exact`, and the `consumed` field, match `core_read.cpp`: `DecodeTx` rejects leftovers, while `DecodeHexBlk` and `DecodeHexBlockHeader` ignore them.
- **`is_coinbase`.** Core is `vin.size() == 1 && prevout.IsNull()` (`transaction.h:308-311`, identical on both pins); base-sdk's differs, so §5's choice is correct.
- **Release-dependent txid.** The `UngatedRules` keying, the `Tx`/decoder plumbing and "v23.1.8's instance hash is the txid" match Core and O1's `#if CLIENT_VERSION_MAJOR >= 24`.
- **`ValidationError`.** The discriminants match `consensus/validation.h`, identical on both pins. Splitting the tx and block types and using a `Cow<'static, str>` reason is right (D4), with m-1.
- **Payload slots per BLS scheme.** Justified by A1 §1.4.1's "global" scheme source for CbTx and asset unlocks. The txid never depends on the scheme, because it hashes raw payload bytes.
- **Performance.** One decode pass, hashing straight from the wire, an eager merkle root through `hash_64_many`, and `Arc` per transaction are all on par with Core's `DeserializeBlockTest` plus `BlockMerkleRoot`. The `hash.rs` seam is the right place for a SHA-NI-less backend swap. With M-3, add the comparison pass to the bench.
- **Over-engineering.** Little. The `ResultClass` trait could be two concrete types made by a macro, but D4 settled the shape. The `ConsensusCtx` and view skeletons are justified by EV0's API review.
- **Interfaces.**
  - EV0 and CS3 get what they need (`Tx`, `Block`, predicates, `size`, merkle with the mutation flag, txids for the CheckBlock debug message).
  - CS1 gets the field writers (plus DEC-25's two).
  - CS4 gets `Deployment` and `DeploymentPos`, which match `consensus/params.h` on both pins.
  - CH1 gets `decode_header`, `HeaderCtx` and `ChainAncestry` (with m-4).
  - CS5a gets `CoreWrite` and the compact-size writer.

## Disposition table

| ID | Severity | Section to change |
|---|---|---|
| B-1 | Blocker | §9.4, §10, §13 D7 (and the private note) |
| M-1 | Major | §9.4, §13 D6 |
| M-2 | Major | §9.3, §9.5, §10 corpus layout |
| M-3 | Major | §4.3, §6.1 (`Decoded` doc), §6.2, §9.2 |
| M-4 | Major | §9.1 |
| M-5 | Major | §4.3, §5 |
| M-6 | Major | §2, §11 (consumers), `ARCHITECTURE.md` |
| P-M1 to P-M6 | Major | private note (see private review) |
| m-1 to m-12 | Minor | as listed |
