# EV1-2 code review, round 1 (Opus)

- **Branch:** `node/ev1-2` at **0e8f2f3e9** (frozen worktree `/work/scratch/ev1-2-frozen`), net diff `fb76254e0..0e8f2f3e9`.
- **Pin:** Core v24.0.0-rc.4 (tag 49caf372bb → 0e7382df80). base-sdk f27bc4f5.
- **Reviewer scratch:** `/work/scratch/ev1-2-rv-opus1` (reflink copy with env-gated mutants), `/work/scratch/ev1-2-rv-opus1-target`, `/work/scratch/ev1-2-rv-opus1-core` (rc.4 sources), `/work/scratch/ev1-2-rv-opus1-bsdk` (base-sdk pin export). I did not read the Sol review.

## Verdict: ACCEPT-WITH-NITS

I compared every step of `trivial.rs` against `providertx.cpp` rc.4, `validated_payload` against `GetValidatedPayload` (`specialtxman.cpp` rc.4:1157-1184), and the ECDSA paths against `messagesigner.cpp` and `pubkey.cpp`. I found no path that accepts a payload Core rejects, or rejects one Core accepts. The base-sdk builders behind the preimages follow Core's re-serialization, including the cases that are easy to get wrong:

- a legacy `MnNetInfo` with an invalid service re-encodes as `CService{}` (`netinfo.h` rc.4:343-348; base-sdk `NetInfoV1::decode`);
- the lazy operator key re-serializes its stored bytes after a successful `Get()`;
- the consent hash writes the share count as a byte.

No finding blocks the merge. m-1 adds four or five L0 vectors and should land before merge, as one small commit that needs no re-review. The other minors can ride with EV1-3.

Counts: 0 M, 5 m, 5 n.

## Findings

### m-1: Four accept-vs-reject predicates survive every EV1-2 test, and the V24 version gate is pinned only through types 10–12

The 210/0 cargo-mutants result covers only the operator swaps cargo-mutants generates. I hand-wrote 16 env-gated mutants of the riskiest behaviours (list under "What I ran"). Four of them change a reject into an accept, and no test in `dash-dml` or `node-l1` notices:

| Mutant | Rust | Core | Effect if it were real |
|---|---|---|---|
| M12: duplicate-payout check against the previous payout only | `trivial.rs:211` | `providertx.cpp` rc.4:43-44 (`std::set`) | payouts `[A, B, A]` with rewards summing to 10000 are accepted where Core gives `bad-protx-payee-dup`. The only vector (`trivial/tests.rs:288`) is the adjacent `[A, A]` |
| M13: duplicate-refund check against share 0 only | `trivial.rs:259` | `:91-92` | shares `[A, B, B']` (B' reusing B's refund script) are accepted where Core gives `bad-protx-shares-dup-refund`. The vector (`tests.rs:296`) duplicates share 0 |
| M6: 3e.iii (`dest == keyIDVoting`) skipped for the reward script | `trivial.rs:272` | `:107-108`, which runs for both scripts | a share whose non-empty `scriptReward` pays the voting key is accepted where Core gives `bad-protx-shares-payee-reuse`. Only the refund-script arm has a vector (`tests.rs:298`) |
| M8: `ExtractDestination` accepts a 65-byte P2PK whose header is not 4/6/7 (e.g. 5) | `trivial.rs:134-143` | `pubkey.h:59-66, 77` (`ValidSize`), `standard.cpp:42-53` | through EV1-3, an external-collateral coin `<65 bytes, header 5> OP_CHECKSIG` would pass `bad-protx-collateral-dest`/`-pkh` where Core rejects. `destinations()` (`tests.rs:494`) has only the positive 4/6/7 cases and the 33-byte-with-header-4 negative |

Three more mutants are caught only by the node-l1 oracle test `shared_types_match_core_across_v24`, and only through types 10–12. No L0 vector catches them:

- **M1:** `validated_payload` ignores `v24` in the version maximum, so a version-3 ProTx is accepted with V19 active and V24 not.
- **M3:** V24 is treated as implying V19.
- **M7:** ProUpShare accepts `vch_sig.len() > 65`. The vector at `tests.rs:353` covers only 64 bytes.

M1 is the V24 deployment boundary for types 1–4 (ExtAddr ProReg/ProUpServ/ProUpReg/ProUpRev before activation → `bad-protx-version`, `specialtxman.cpp` rc.4:1173-1174). It is caught today only because a version-3 ProUpShareTx happens to report `bad-protx-version` instead of `bad-proupshare-version`. `version_above_the_tips_maximum` (`payload/tests.rs:177`) tests only `(v19, v24) = (false, false)`.

**Fix (L0, about 30 lines):**

- payouts `[A, B, A]` → `-payee-dup`;
- shares `[A, B, C]` with C's refund equal to B's → `-dup-refund`;
- a share whose refund script is clean and whose `script_reward` is P2PKH(voting) → `-shares-payee-reuse`;
- `extract_destination` returns `None` for a 65-byte push with headers 0/2/3/5 and for a 33-byte push with headers 6/7;
- ProUpShare with a 66-byte `vch_sig` → `-sig-size`;
- in `payload/tests.rs`, a valid v3 payload of each of types 1–4 under `(true, false)` → `bad-protx-version`, and under `(true, true)` → `Ok`.

Mutants M6, M8, M12 and M13 then need a re-run to confirm they are killed.

### m-2: The §3.18 `CalcTxInputsHash` row (`sig::inputs_hash`) is not in the slice

- **Where:** `dash-dml/src/specialtx/sig.rs`. The design's §3.18 lists `CalcTxInputsHash | providertx.h | sig::inputs_hash (stream every prevout)`, and the EV1-2 row ships "preimages (§3.18 rows except the MNHF/AU ones)". Nothing in `dash-dml` computes it.
- **Core:** `providertx.h` rc.4:559-566 (`CheckInputsHash`) feeds `bad-protx-inputs-hash` in every ProTx check (types 1–4, 11, 12). base-sdk computes it only inside `shared_consent_bytes`.
- **Scenario:** EV1-3 writes its own prevout hash for `CheckInputsHash`. That hash has no L0 cross-check: the §6.1 "every preimage against `special_tx_sign_hash`" claim does not cover it, and an encoding slip such as hashing `vin` with scriptSigs, or sha256 instead of sha256d, would surface only at EV1-3's L1.
- **Fix:** add `sig::inputs_hash(tx) -> [u8; 32]`. Use as an L0 vector that every BS6b Core-signed record's payload `inputs_hash` equals `inputs_hash(tx)` (117 free vectors). Or record a D-row that moves the row to EV1-3 with that same vector.

### m-3: The preimage and ECDSA API lets EV1-3 skip a signature check without a compiler warning

- **Where:** `sig.rs:32, 45, 54` return bare `bool` with no `#[must_use]`. `sig.rs:88, 96, 103` return `Option`, where `None` is "unreachable after `validated_payload`".
- **Scenario:** `verify_hash_canonical(&h, &owner, sig);` written as a statement compiles silently. `if let Some(h) = payload_sig_hash(p) { if !verify_hash(..) { reject } }` passes a payload whose hash is `None`. The `None` arm is unreachable today, because base-sdk refuses only versions outside 1..=3 (`supported_version`, `payload/mod.rs:99`) and `validated_payload` rejects those first. But the API leaves the reasoning to every call site, in four types and two slices (EV1-3, EV1-4).
- **Fix:** in order of preference:
  1. Land Core-shaped wrappers now, as §3.1 names them: `check_hash_sig_ecdsa`, `check_string_sig`, and a `check_hash_sig_canonical(hash, key, sig, reason)`. Each returns `Result<(), TxValidationError>` with Core's class and reason, and folds the `Option` in one place (`None` → the same reject, with an invariant comment).
  2. Or make the preimage functions infallible on a `Validated<'a, T>` that only `validated_payload` constructs.
  3. At minimum, `#[must_use]` on all seven functions.

### m-4: `SigVerifier`'s verify scheme has the same type as the decode scheme

- **Where:** `sig.rs:130-136`. `scheme: BlsSchemeKind` is the global flag at the call (`bls.cpp:296-312`), and `BlsEagerSig::scheme()` and `BlsLazyPk::scheme()` return the same type.
- **Scenario:** in EV1-4, `verifier.bls_verify_insecure(&p.sig, &pk, &h, p.sig.scheme())` type-checks. It verifies a v1 ProUpServ signature under the legacy scheme after V19, where Core uses the global basic flag. That is the quirk-21 confusion D22 was written to prevent.
- **Fix:** take the verify scheme as a distinct type that only the consensus context produces (a `VerifyScheme` newtype from `BlsCtx`/`cons.bls`, or `&BlsCtx` itself), so the decode scheme cannot be passed. This can land with EV1-4. Record it on the EV1-4 row if it is deferred.

### m-5: The D22 vectors compare only "decodes or not"; they don't pin that a foreign-scheme signature decodes to the invalid object

- **Where:** `node-l1/tests/l1_specialtx_sig.rs:223` (`let ours = matches!(raw.payload(), Some(Ok(d)) if d.is_complete())`). The test counts Core's `Reset()` through `d.reencoded` but never inspects our decoded `sig`.
- **Core:** `bls.h` rc.4:184-203. A signature canonical only in the other scheme decodes, then is `Reset()`, so `VerifyInsecure` is false and the result is `bad-protx-sig` once `check_sigs` is on.
- **Scenario:** if base-sdk's `BlsEager::decode` ever kept the other-scheme point (`wrap_eager.rs:64-75` is right today), every EV1-2 test still passes. With EV1-4's verifier, a v1 ProUpServ carrying a basic-scheme signature of the operator's key could then verify, and be accepted where Core rejects.
- **Fix:** in the same loop, assert that `ours` (the decoded `ProUpServTx.sig` / `ProUpRevTx.sig`) has `!is_valid()` for the "other" case, and that our re-encoding of the payload equals `d.reencoded`. Each is a two-line addition.

### n-1: The generated-preimage leg leaves most consent and ProDis inputs fixed

`l1_specialtx_sig.rs:421-490, 514`. `mutate` never touches:

- `vin` (prevouts, sequences, count) or `vout`, which `MakeSharedRegConsentHash` and `CProDisTx::MakeSignHash` hash field by field (`providertx.cpp` rc.4:335-373, 496-523);
- `netInfo`, `pubKeyOperator`, `nType` or the platform fields of types 1–3.

Only the lock time and some scalar payload fields vary, so "1,000 generated payloads per type" exercises the vin/vout layout only as the 117 Core-signed records have it. **Fix:** also draw the input count (1–3), the prevouts, the sequences and the output values and scripts, and flip `mn_type` between Regular and Evo on v2/v3 records.

### n-2: The EV2b scaffold re-implements trivial steps T5–T7 inline

`node-l1/src/dml_checks.rs` (`pro_reg`, the T5 `if` chains; `operator_keys`) re-implements these steps instead of calling `trivial::pro_reg_trivially_valid` / `pro_up_reg_trivially_valid`. The scaffold walk can drift from the production code. Since T8 already calls `is_payout_list_trivially_valid`, take T5–T7's verdicts from the production functions too, or add a comment that ties them to `trivial.rs:367-398`.

### n-3: `validated_payload` takes `tx_type` and `slot` as unrelated arguments

`payload.rs:97-102`. A caller can pair one transaction's type with another's slot. Taking `(tx: &Tx, slot: PayloadSlot)` and debug-asserting `slot` against `tx.payload()` would remove that freedom without changing the CH7a/`BlockPayloads` flow.

### n-4: No vector for `(v19, v24) = (false, true)` through `validated_payload`

`version_max_follows_the_forks` pins `version_max(false, true) == 1` (`payload/tests.rs:84`), but no vector calls `validated_payload` in that state. Core's `GetMax` comment (`types.h` rc.4:26-30) says the state is real on non-standard configs. Mutant M3 is caught only incidentally by the type-12 oracle test. One L0 line fixes it.

### n-5: Small doc nits

- base-sdk's `RevocationReason` names (`KeyCompromise=1`, `ChangeOfKeys=2`, `ViolationOfService=3`) don't match Core's (`TERMINATION_OF_SERVICE=1`, `COMPROMISED_KEYS=2`, `CHANGE_OF_KEYS=3`, `providertx.h` rc.4:388-393). The values match, so this is harmless for consensus, but it belongs in a BSX catalogue line.
- Add a vector that reason 3 passes and 4 fails from wire bytes, which also pins base-sdk's mapping.

## The focus questions

1. **Deployment states and version bounds.** `validated_payload` is a pure function of `(v19, v24)`; its order is type, then full decode, then unknown-version, then the maximum, then `IsTriviallyValid`. That matches Core for every combination, including types 10–12, whose readers always decode in full: `shared_types_version_boundary` and the oracle test over 24 BS6b records × 17 variants × tip × V24 state.
   - The boundary heights belong to the caller. EV1-3 must compute `v19` as `DeploymentActiveAfter(cons.prev, V19)`, not from `rules`, and pass `rules.v24` for `v24` (Core passes `rules.v24` / `is_v24_active` into GVP, `specialtxman.cpp` rc.4:1212, :1387).
   - It should test both at `tip = activation − 1` and at `activation`. The evo_trivialvalidation runner uses tips 98 and 100 and skips the exact boundary 99.
   - Pinning the V24 gate for types 1–4 at L0 is m-1.
2. **SigVerifier and ECDSA seam misuse.** The BLS seam fails closed: `FixedVerifier::default()` is `VerifyUnavailable` everywhere, and the trait returns `Result`. The ECDSA paths have no seam: they are free functions returning `bool`/`Option`, so a skip is a one-line call-site slip. That is m-3; the decode/verify scheme type confusion is m-4.
3. **Preimages.** `ProReg`/`ProUpServ`/`ProUpReg`/`ProUpRev`/11/12 `SerializeHash`, `MakeSignString`, `MakeSharedRegConsentHash` and `CProDisTx::MakeSignHash` match Core's serialization (I read base-sdk `proregtx.rs:296-421` and `write_net_info` against `providertx.h`/`.cpp` rc.4). The oracle test confirms them on 117 Core-signed records plus the generated leg. Gaps: `CalcTxInputsHash` is missing (m-2), and the generator doesn't vary vin/vout (n-1).
4. **API for EV1-3 and CH7a.**
   - `BlockPayloads::{new, from_txs, get, len, is_empty}` is enough for CH7a-2b and EV2b, because each `Tx` caches its decode. An `iter()`/`as_slice()` would save CH7a an index loop; that's optional.
   - EV1-3 has what it needs for types 1, 3 and 10–12: `validated_payload`, `OwnerPayouts`, `is_payout_list_key_safe`, `is_share_list_voting_key_safe`, `reward_script`, `collateral_amount`, `extract_destination`/`Destination`, and the ECDSA primitives.
   - EV1-3 still lacks `inputs_hash` (m-2) and the Core-shaped `check_*_sig` wrappers (m-3).

## D-row rulings

The brief's list mixes the implementer's notes rows (`/work/scratch/ev1-2/notes.md`) and design rows. I rule on both where both exist.

| Row | Ruling |
|---|---|
| **D17** (notes: the `netinfo_validate` tx-verdict leg; design D34) | **Accept.** The leg calls the production chain (`validated_payload` → version-disallowed → `check_provider_network_fields`). Its "ours passes ⇒ Core's next step exactly" rule (`bad-protx-hash` / `bad-protx-collateral`) is a strong oracle short of A-3. Template coverage limits are stated (no shared, no multi-payout, so `:1223-1230` never fires). Certification is D28g's, and I did not redo it. |
| **D17** (design: `key_changed` equality) | **Accept, EV1-3.** EV1-2 has no `key_changed` code. base-sdk's `BlsLazy::eq` at the pin (`wrap_lazy.rs:258-283`) is Core's `bls.h:514-535` order. The design's line cite `@dbe245f7 :252-284` is stale for the f27bc4f5 pin; refresh it when EV1-3 lands the vectors. |
| **D19** (notes: `seeds_decode_and_hold` repaired) | **Accept.** It is a pre-existing break from R6; the seed stays as a refusal case. "node-fuzz lib tests are not in tier 1" deserves its own backlog row (BL-459 is a lock check, not this). |
| **D19** (design: per-element order, pairwise) | **Accept.** The loops in `trivial.rs` run each element's steps in Core's order (`share_loop_order`). Pairwise evidence for the `IsTriviallyValid` and §3.14 tables arrives only with EV1-3's L1; until then, ordering inside the trivial tables rests on single-fault vectors. EV1-3's pair-coverage report must list the §3.3–§3.9 trivial tables and §3.14 explicitly. |
| **D20** (notes: 9 netinfo seeds) | **Accept.** |
| **D20** (design: unknown ProTx version) | **Accept.** L0 (`unknown_versions_d20`) and the oracle (`unknown_versions_match_core_d20`) agree. The covenant half is EV1-3's. |
| **D22** (notes: validator r2 applied) | **Accept.** |
| **D22** (design: BLS schemes) | **Accept, with m-5.** It uses `oracle.payload_decode` rather than `check_special_tx`; that is equivalent for the decode verdict, and acceptable. |
| **D23** (notes: tier 1 + bench_inputs) | **Accept.** PF0 PASS with the approved dev-dep change (DEC-601). |
| **D23 / D33** (design: preimages over non-canonical wire forms at rc.4) | **Accept.** rc.4's `ExtNetInfo::Unserialize` throws on unknown or out-of-order purposes (`netinfo.h` rc.4:425-433, verified), so D23's original forms cannot reach a preimage. The substitutes (an entry Core clears, `0.0.0.0`) are the right ones. D33 is correctly documented. |
| **D25** (notes: `ev1-pairs.toml` moves to EV1-3) | **Accept.** Nothing in EV1-2 measures pair coverage, and the file's exclusions name steps of tables EV1-3 lands. Record the move as a design D-row (amend §6.2 "checked in with EV1-2" and the EV1-3 §7 row), so the next review does not flag it. |
| **D25** (design: `CoreThrow`) | **Not in EV1-2's scope (EV1-5).** |
| **D29** (design: `netinfo_validate` tx-verdict certifies at EV1-2) | **Accept the harness. The certificate is pending under D28g.** The §7 row text "certified … (D29, D34)" is accurate only once D28g resolves; EV1-2 should not be marked done on that line until then. |
| **D32** (census at rc.4) | **Accept.** I reran `ev1-census.py --core ~/workspace/dash --check`: 279 rows match v24.0.0-rc.4. The three new `unreachable` rows (`providertx.cpp:304/462/573`) are right: the readers fix the key scheme from the version (`providertx.h` rc.4:180, 346) or as basic (`:534`), so `IsLegacy()` cannot disagree, and a bad-scheme key fails `Get()` first (`:301`, `bad-protx-key-null`). |
| **Notes D6 / BL-460** (allocating builders vs §3.18's streaming) | **Accept as performance only.** PF0 shows no regression. |

## What I ran

- `cargo test -p dash-dml --lib specialtx` on the reflink copy: 33 passed.
- `cargo test -p node-l1 --features oracle --test l1_specialtx_trivial --test l1_specialtx_sig`: 3 + 5 passed.
- `contrib/node/ev1-census.py --core ~/workspace/dash --check`: 279 rows match rc.4.
- 16 hand-written env-gated mutants (`EVMUT=n`, one build, only in `/work/scratch/ev1-2-rv-opus1`):
  - M1, M3: the V24 gate in `validated_payload`;
  - M2: trivially-valid before the version maximum;
  - M4, M5, M6, M13: share-list penalty, 3e.iv, 3e.iii-on-reward, and duplicate refund;
  - M7: ProUpShare sig size;
  - M8: P2PK header;
  - M9, M10, M11: canonical header bound, low-S on R, and no low-S;
  - M12: non-adjacent payout duplicate;
  - M14: null-owner skip;
  - M15: type-12 legacy key;
  - M16: shares-external.
- Mutant results:
  - Killed by L0: M2, M4, M5, M9, M10, M11, M14, M15, M16.
  - Killed only by the node-l1 oracle test: M1, M3, M7.
  - Survived everything: M6, M8, M12, M13 (m-1).
- Read against rc.4: `providertx.cpp` (all), `providertx.h` serializers, `specialtxman.cpp:1128-1240`, `bls.h` (`CBLSLazyWrapper`, version wrappers, `Unserialize`), `netinfo.h` (`MnNetInfo`, `NetInfoEntry`, `ExtNetInfo` serialization), `messagesigner.cpp`, `pubkey.cpp` `RecoverCompact`, `pubkey.h` `GetLen`, `standard.cpp` `Solver`.
- Read in base-sdk at f27bc4f5: `proregtx.rs` builders, `write_net_info`, `NetInfoV1::decode`, `BlsEager::decode`, `BlsLazy::eq`, `RevocationReason`.
- Not redone: fuzz certification (D28g), PF0, tier 1.
