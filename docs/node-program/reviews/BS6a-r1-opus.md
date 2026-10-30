# BS6a review r1 (Opus): v24 payload shapes, BLS fields by scheme source

**Verdict: APPROVE-WITH-NITS.** base-sdk `node/bsdk-bs6a-v24-shapes` at 9094d81 can merge into `node/base-sdk` as it stands. The rust-dashcore re-pin is a separate step, and it must not land until it carries finding 1 (DEC-68 is not implemented anywhere yet, so `check-pure.sh` fails at this pin) and follows the commit order in finding 2.

Scope:
- base-sdk 481f8c3..9094d81 (14 commits);
- rust-dashcore `node/bs6a-adapt` (4 commits on 30be7902);
- the implementer's evidence in /work/scratch/bs6a/evidence.

The GPT reviewer covers field-level Core equivalence. This review re-verifies the evidence independently and looks at API, conventions and the merge plan. My scratch is in /work/scratch/bs6a-opus/.

## Re-verification (a)

| Check | Result |
|---|---|
| L0 `trivial.rs` on my build of 9094d81 | pass. The corpus is **verbatim** Core: `git show` of `src/test/data/trivially_{valid,invalid}.json` at v23.1.8 (728f505583) and v24.0.0-rc.3 (0364ed626e) equals `trivial.json5` row for row (25 + 12 per pin) |
| `payload_core.rs` (84 crafted cases, 4 tests) | pass |
| **Re-judging the 84 cases.** I downloaded both x86_64 release tarballs myself and checked them against `SHA256SUMS.asc`. Four private regtest dashds (each pin × basic/legacy flag, /dev/shm datadirs, ports 39511–39514) ran `decoderawtransaction` on every raw case | **336 of 336** recorded Core outcomes reproduced exactly |
| **Independent probe.** My own payload builders and BLS samples (the *last* row of each class in `bls_wrapper.json5`, plus classes gen.py does not use: `off_100`, `nonsub_000`, `x_max_000`, `x_ge_p_100`, `stray_011_sub`, `imag_x_legacy_s0`, `nonsub_legacy_s1`, `sub_legacy_s0_basic`, `fp_x_basic_s1`, `legacy_c1_ge_p`). 119 cases: ProReg/ProUpReg v1/v2 lazy keys; ProUpServ/ProUpRev v1/v2 sigs; QC v1, v2, v3, v4, v5 and 0xffff (all fields legacy-only, basic-only, both, or a throw in the first or last field); CbTx v3 and v5 in both layouts; trailing bytes; MnHF; AU v0–v3; type 12. Compared with the existing `compare()` against v24 under both flags | **238 of 238** case-flag pairs match: decode verdict and every printed field. The only pin-dependent cases are type 12 and CbTx v5, both expected (finding 3) |
| Flag mechanism (`-testactivationheight=v19@1000`) | confirmed by the data: a legacy-only CbTx/AU signature prints valid under one flag and zeros under the other, at both pins |
| Decode-verdict symmetry | across all 812 Core outcomes (203 cases × 2 pins × 2 flags), no case decodes under one flag and fails under the other. This matches Core's algorithm: THROW ⇔ canonical in neither scheme, which is symmetric in F. So `Transaction::check()`'s claim that its verdict does not depend on the scheme (`transaction.rs:338`) holds |
| Bench (`instructions:u`, 34-block corpus × 200, net of the load) | block decode: 481f8c3 894,982,066 vs 9094d81 894,975,703, **−0.0007%**, reproducing the implementer's −0.0008%. Payload decode: about 990× (1.65M → 1.63G per 20 iterations), all of it eager G2 decompression and subgroup checks. This is informational; Core does the same work in `GetTxPayload` |
| **BS2 on testnet** through the harness's read-only `getblock` path to the shared oracle (3 threads, nice 19 / ionice idle), every block cross-checked against `getblock 2` (`--verbose-types 1..12`, which includes QcTx) | 845,000–860,999, spanning V19 (850,100) and the QC v2→v4 switch: 16,000 blocks, 10,526 QcTx, **0 mismatches**. 1,528,000–1,568,812: 40,813 blocks, 21,017 QcTx, 545 AU, 3,035 AL, **0 mismatches**. Gate exit 0 for both |
| base-sdk clippy `-D warnings` (primitives + types, full and no-default), nextest (primitives, types, pkc: 866 passed), `cargo doc -D warnings` | pass |
| `lint_commit.py -r 481f8c3..9094d81` | 14/14 pass. All 14 base-sdk and 4 rdc commits carry SSH signatures |
| rdc `cargo test -p dash-conformance` at the local pin | pass (74 tests) |
| rdc `check-pure.sh` at the local pin | **fails**: `threadpool` is banned for `dash-consensus` (finding 1) |
| CodeQL evidence (`codeql-final.log`) | check exit 0, run exit 0, 242 files, 6 policy queries. It ran; this is not an exit-77 skip |

## Findings

### 1. major (re-pin plan): DEC-68 is not implemented, and its clippy ban cannot target blst

**Where:** `contrib/node/pure-deny.toml:46` and `pure-clippy.toml` on integration; DEC-68.

**Failure scenario.** The plan is "`bump-base-sdk.sh`, then land the 4 adapt commits". The script's gates run `check-pure.sh`, which fails today (reproduced: `crate 'threadpool = 1.8.1' is explicitly banned` for dash-consensus). None of the four adapt commits touches `contrib/node`. DEC-68 also asks for a clippy `disallowed_methods` ban on "blst's thread-spawning entry points in pure crates", which cannot work as worded:
- Pure crates never call blst. blst is not their dependency; they reach it only through dash-pkc. Clippy lints only the calls in the crate it checks, so a ban on `blst::…` paths can never fire.
- Every pooled path in blst 0.3.17 is compiled only with blst's own `std` feature. That covers `mod mt`/`da_pool`, the `std` variants of `aggregate_verify` and `verify_multiple_aggregate_signatures`, `miller_loop_n`, and `pippenger.rs`. **Nothing in the rdc graph enables `blst/std`**, even with `--all-features` (`cargo tree --all-features -e features -i blst` lists no blst features). So `threadpool` is linked but unreachable today.
- The real risk is feature unification. A non-pure crate that DEC-68 itself envisages (parallel batch verification in LLMQ/sig workers) would enable `blst/std`. dash-pkc's `BlsSignature::verify_aggregates` (`scheme_ietf.rs:177`, which calls blst `aggregate_verify`) would then silently become multi-threaded inside a pure crate. A ban on blst methods would not see that.

**Fix (in the re-pin commit):**
- (a) Add the `wrappers = ["blst"]` exception.
- (b) In `check-pure.sh`, assert that `blst` resolves without its `std` feature for each pure crate *and* for the node binary's graph. That makes the X-1 call-level rule hold by construction.
- (c) If a non-pure crate ever needs `blst/std`, put the dash-pkc wrappers that reach pooled paths into `pure-clippy.toml` (`dash_pkc::bls::BlsSignature::verify_aggregates`, the legacy scheme's equivalent if it reaches a pooled blst path, and any later multi-verify wrapper).
- (d) Add a selftest case for (b).

The manager should restate DEC-68 in these terms.

### 2. minor (re-pin plan): commit order, nested pins, and the CS0 call site

- **Order.** No adapt commit builds at the pin it is committed with: 30be7902 pins 481f8c3, and the adapt commits already call `decode_payload(ORACLE_BLS)`. The branch is not bisectable. Put the re-pin commit (manifests and locks) *before* the adapt commits.
- **dde-probe.** The report says `bump-base-sdk.sh` "also moves the nested cs2-diff and dde-probe pins". It does not move dde-probe, and should not. `contrib/node/dde-probe/{bench,oracle}` are standalone tools with fixed old/new git revs (33fcc59 vs 0c71f50) and no `[patch]`; `base_sdk_pins.py nested` lists only `cs2-diff` on integration. After CS0 merges it will also list `node-fuzz`.
- **CS0 call site.** CS0's `Tx::payload` calls `self.raw.decode_payload()` (`dash-consensus/src/primitives/tx.rs:149` on `node/cs0-node-primitives`). The doc comment above it says "base-sdk's payload decoder takes no scheme yet". Whichever of CS0 and the re-pin lands second must change both to `decode_payload(bls)`. That is one line; no other rdc code on integration or CS0 calls the changed APIs (grep checked).

**Recommended order (d):**
1. Merge BS6a into `node/base-sdk` now. BS5 and BS7 share no files with it (I compared `git diff --name-only`), and neither uses the re-typed payload fields or matches `DecodeError` exhaustively, so they simply rebase.
2. **CS0 merges first, at 481f8c3.** It is ★, in its final rebase, an EMBARGO-1 designated reader, and its gates are expensive (BS2 consensus leg, `check-private-tests.sh`, node-fuzz). Moving its base again would restart them, for a one-line API change.
3. Then one re-pin branch off post-CS0 integration:
   - commit A: `bump-base-sdk.sh --no-gates` (root, `cs2-diff`, `node-fuzz`), CS0's call site and doc, and DEC-68 per finding 1;
   - then the 4 adapt commits rebased on top;
   - then the full gates: the script's gates, `node-fuzz/run.sh` build, `selftest-checks.sh`, and the dashd suites.
4. EV0 (CS0 + i:BS6a) can code against `node/base-sdk` types without waiting for the rdc re-pin. If CS0 slips by more than about two days, re-pin first and give CS0 the one-line change, accepting a re-run of its gates.

### 3. minor (API): the decode signature is not final; SIG-05 needs an owner and an API

**Where:** `payload/mod.rs:510`; `transaction.rs` `decode_payload(bls)`; `PayloadError` at `mod.rs:423-427`.

CS0 says `Tx::payload(bls)` "already has the final shape". Two more decode inputs are coming:
- BS6c's v23.1 ProTx layout and DomainPort strictness (EVO-14).
- SIG-05/EVO-06. O17 lists BS6a as an EVO-06 consumer, and LQ1's coinbase-CL predicate "decodes with BS6a's CbTx decoder under rules (cbtx_v4_known) and bls".

The report leaves SIG-05 to "the SIG-05 owner, a decoder parameter or derived from `PayloadDecode`". Derivation is not possible with today's API. v23.1 gives `bad-cbtx-version` only when the v3 prefix decodes *and* the payload ends exactly there. v24 reports that case as the same opaque `PayloadError { message: String }` as a malleable signature or an earlier EOF, so telling them apart means matching on a `Display` string.

**Fix:**
- Assign SIG-05 to **BS6c**. Both are "v23.1 reads this payload in an older layout" (S size, same reviewers).
- Make BS6c's parameter one value, e.g. `PayloadLayout { protx_v3: V23_1 | V24, cbtx_v4_known: bool, domain_strict: bool }`, or a `PayloadCtx { bls, layout }`.
- Have `PayloadError` keep the structured `DecodeError` and the offset where it failed, not just a string.
- CS0 §5.1 says nothing release-dependent is cached in `Tx`. Once the layout is a decode input, CS0's two per-scheme slots must be keyed by the full decode context, or `Tx::payload` must take the layout explicitly. Flag this to CS0 and EV0 now, before EV0 freezes interfaces.

### 4. minor (API): the BASIC default of context-free decode; a context-free alternative

**Where:** `cbtx.rs:82`, `assetunlock.rs:68`, `SptxDecode` at `mod.rs:181-189`.

**When the default is wrong.** On valid mainnet and testnet data the BASIC default is always right: CbTx v3 (V20) and asset unlocks only exist after V19. It is wrong only in two cases:
- under the `V23_1` profile's leaked legacy flag (A1 §2.25 row 26, `Effect::BlsSchemeLeak`), for mempool or RPC-time decodes of AU/CbTx;
- on regtest with V19 moved past V20.

So the danger is narrow but consensus-adjacent.

**A clippy ban is not viable.** I tested `disallowed-methods` with agentbox's clippy 0.1.99 (/work/scratch/bs6a-opus/clippyban):
- a path to a specific impl (`dash_primitives::CoinbaseCommitment::decode`) "does not refer to a reachable function";
- the trait-level `dash_types::codec::BaseCodec::decode` also bans `Transaction::decode`.

**Options:**
- (a) A textual check in `check-pure.sh` for `CoinbaseCommitment::decode`, `AssetUnlock::decode` and `<… as BaseCodec>::decode` on them, in pure crates. Cheap, but fragile.
- (b) **Type-level, recommended before EV0 freezes.** Because THROW is scheme-independent (see the symmetry row above), a global-sourced field needs no context at decode time:
  - decode checks "canonical in some scheme" and keeps the raw bytes;
  - the value is resolved per `BlsCtx` where it is read: `best_cl_signature(bls)`, `quorum_sig(bls)`. Only EV4/LQ1's CbTx-CL check and the AU quorum-sig check read them.
  - This removes `BlsCtx` from `SpecialPayload::decode`/`decode_payload`, removes `SptxDecode`, removes CS0's second payload slot, and removes the BASIC footgun by construction.
  - Neither Core nor the node re-serializes a decoded CbTx/AU payload for a hash (the txid is over the raw `vExtraPayload`), so deferring the resolution is not observable.

If BS6a keeps the current shape (it is correct), do (a) and record (b) as a considered alternative in A1 §1.4.1.

### 5. minor: `dash-primitives` now hard-requires blst for every consumer

**Where:** `pkgs/primitives/Cargo.toml:43`.

This is fine for the node. It matters when BS14/CV3 bring `dash-primitives` under `dashcore`:
- dash-spv, the key-wallet FFI (iOS/Android) and Platform would all need a C toolchain, and a wasm32-capable clang for wasm consumers.
- `Transaction::check()` now runs eager BLS decodes, about 1.2M instructions per G2 field and about 3.6M per QcTx. A consumer that calls `check()` and then `decode_payload()` pays twice.

Record this in the BACKLOG against BS14/CV3, and offer a `bls` feature (default on) when upstreaming.

### 6. nit (semver): `DecodeError::MalleableBls` is a breaking change to `dash-types`

**Where:** `pkgs/types/src/codec.rs:55`, commit 03384e1.

`dash-types` 0.1.0-beta is published, and `DecodeError` is an exhaustive public enum. A new variant breaks exhaustive matches; `DecodeError::map` itself had to grow an arm. base-sdk marks breaking changes with `!` (e.g. `0ba7bf5 types%refac!`), so this should be `types%feat!`, with the CHANGELOG entry under "Changed" telling callers to add an arm.

Better: mark `DecodeError` `#[non_exhaustive]` in the same change. BS7 and BS8 will need more variants. Unreleased already holds breaking `dash-types` changes, so the cost is the label, not the release.

### 7. nit: expose the version-to-scheme mapping

**Where:** `payload/bls.rs:27,33` (`pub(crate)`).

The adapt branch already duplicates it (`conformance/src/payload.rs:260-267`, `protx_scheme`/`qc_scheme`). BS8 needs it for SML entry v1 and commitments in `mnlistdiff`/`qfcommit`, and EV2 for "legacy iff state nVersion == 1". Several copies can drift apart. Make them public (e.g. `ProTxVersion::bls_scheme(v)`, `Commitment::bls_scheme(v)`), as the report's optional item suggests.

### 8. nit: `SptxDecode::decode_sptx` has a default body

**Where:** `mod.rs:187`.

The default forwards to `BaseCodec::decode`. A future payload type with a global-sourced field that forgets to override it silently decodes under BASIC. Make the method required (ten one-line impls), or add a test that enumerates the global-sourced fields per type.

### 9. nit: lazy fields re-encode depending on call history

**Where:** `bls.rs:90` `write_lazy`, and the serde proxies (`ProRegTxSerde` etc. use `to_bytes()`).

After a failed `get()`, which `check()` and `==` trigger, `encode`, `Hashable::hash` and the JSON of a payload with an invalid lazy key change from the raw bytes to zeros. That is Core's re-serialization rule, and it cannot move a txid (`core_serialize` writes the raw `extra_payload`). It is surprising for JSON and wallet re-encoding, though. Document it on `ProRegTx`/`ProUpRegTx`/`ProUpSharedRegTx`, or have the serde form carry the wire bytes.

### 10. (withdrawn)

Withdrawn from this file under PLAN §7 handling; see the manager.

### 11. minor: follow-up list (e) is incomplete

The implementer's items are correct:
- BS6c, whose vectors exist: the `proregtx_v23_layout` section and the pin-dependent cases;
- EV1's v24 list rules, `bad-protx-payee-reuse` at any version, and the never-emitted `OperatorKeyMismatch`;
- the A1 §1.4.1 MnHF row (eager, always basic, printed under the flag);
- DDE-SWEEP §5 closure;
- making the helpers public.

Add these:
1. **DEC-68 implementation** at the re-pin (finding 1).
2. **CS0 call site and doc** (finding 2).
3. **SIG-05 owner**: BS6c, with the API in finding 3. LQ1a's coinbase-CL predicate depends on it.
4. **base-sdk AU v2 txid.** It is the one remaining harness finding: `feature_asset_locks` gives `tx.txid`, `block.merkle_root` and `block.check: bad-merkle-root` on 5 valid v24 blocks. base-sdk's `Transaction::hash()` and `Block::check()` reject valid v24 blocks for non-node consumers. Name the owner explicitly: BS6b's "AU v2 stable-txid vectors per profile", or a profile parameter in base-sdk. CS0 covers only the node.
5. **EV1: deployment-capped versions.** The cap by `GetMax(is_basic_scheme_active, is_extended_addr)` is EV1's. `trivial.rs` skips those rows, but the list does not mention them.
6. **BACKLOG BL-25:** close its base-sdk part. The adapt branch now compares payouts, shares and `merkleRootAssetUnlocks`. TORv3 rendering and unknown-purpose keys remain.
7. **BS14/CV3:** blst hard dependency and double-decode cost (finding 5).
8. **CS0/EV0:** decode-context growth versus CS0 §5.1 (finding 3), and the alternative in finding 4.

## API assessment (b)

| Item | Assessment |
|---|---|
| `SpecialPayload::decode(tx_type, data, BlsCtx)` / `Transaction::decode_payload(bls)` | Fits CS0 today: one call-site change, and `PayloadDecode` with `consumed`/`len` is what CS0 and EV1 already use. It is not final (finding 3), and there is a simpler context-free option (finding 4) |
| `UnknownProTxVersion` | Right shape. It matches Core's bail-out (2 bytes consumed, so trailing bytes make the decode incomplete and give `bad-protx-payload`; exactly 2 bytes plus `check()` give `bad-protx-version`). It spares EV1 from constructing bogus default structs. The per-type `ProRegTx::decode` errors on unknown versions, which is documented and fine, since consensus goes through `SpecialPayload::decode` |
| `BlsEager`/`BlsLazy` fields | Fit the consumers. `Commitment` keeps `Eq`/`Hash`, and its `Hashable` is SHA256d over the re-serialization with zeros for invalid fields: Core's `SerializeHash(qc)` (quirk 38), which BS8 and LQ need. `Commitment::is_null` matches `CFinalCommitment::IsNull` (quirk 36). `BlsLazy` gives EV2 what its §3.6 `OperatorKey` contract needs: `CBLSLazyWrapper` equality, stored bytes, `serialize_hash`, `to_bytes_as`, `set`. The decoded `BlsEager` value carries the point, so EV1 can verify a v1 ProUpServ/ProUpRev signature under the global scheme (quirk 21) |
| Dropping `Eq`/`Hash` | Correct and necessary. `BlsLazy::eq` may decode, so it is neither an equivalence nor side-effect free. It is documented on `SpecialPayload` (`mod.rs:344-345`), and no consumer design needs either trait on payloads. `PartialEq` stays derived; comparing two fresh decodes in one scheme is a pure byte compare |
| Private serde proxies with `legacyBls: true` | Fine. The JSON stays backward-compatible, the marker is emitted only when true, its default (basic) is right for all real data, version-sourced fields take their scheme from `version`, and a malleable signature fails `try_from`. See finding 9 for lazy fields |
| `dash-primitives` always enabling `dash-pkc/bls` | Unavoidable while the fields are typed (decode needs blst). See findings 1 and 5 for the consequences |

## Summary

- **Evidence: verified independently.** The L0 vectors are verbatim Core. The 84-case Core-judged corpus reproduces exactly on independently downloaded, checksum-verified binaries (336/336). My own 119-case probe of other BLS classes and shapes matches v24 on all 238 case-flag pairs. The bench reproduces (−0.0007% block decode). Two fresh testnet BS2 slices (56,813 blocks, every block cross-checked against `getblock 2`, including the V19 boundary) show 0 mismatches. I found no decode or re-encode divergence from v24.
- **Code and conventions:** good. Commit lint, signatures, CodeQL exemptions and docs are all in order; the one semver label is finding 6.
- **Merge plan:** BS6a merges into `node/base-sdk` now. CS0 merges first. Then a single re-pin carries DEC-68 (corrected per finding 1), CS0's one-line call-site change, the root, `cs2-diff` and `node-fuzz` pins, and the adapt commits, in that order.
- **Before EV0 and BS6c freeze interfaces,** decide two design points: one decode-context value that covers BLS, the ProTx layout and the CbTx v4 layout, with SIG-05 assigned to BS6c (finding 3); and whether global-sourced BLS fields should resolve at use rather than at decode (finding 4).
