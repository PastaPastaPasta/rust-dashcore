# BS11a: the legacy BLS decode domain and Exotic semantics (design)

| | |
|---|---|
| Task | BS11a (PLAN §4 row; A1 §1.4.1), design gate D |
| Branch | rdc `node/bs11a-design`, from `node/integration` 5ae846351 |
| Core | `v24.0.0-rc.3` (the sole parity pin, DEC-82). `v23.1.8` has byte-identical `src/dashbls` decode and arithmetic paths and relic; it differs only by RAII refactors in `schemes.cpp`/`threshold.cpp` and casts in `bls_ies.cpp` |
| relic build | `relic_conf.h` as Core builds it: `ARITH GMP`, `FP_RDC MONTY`, `EP_ADD PROJC`, `EP_MUL LWNAF`, `EP_ENDOM`, `EP_MIXED`, `EP_PRECO`, `EP_WIDTH 4`, `PP_MAP OATEP`, `PP_EXT LAZYR`, `CHECK` off |
| base-sdk | `node/base-sdk` dbe245f (BS11a-t types merged at 098dafd) |
| Builds on | reviews/BS11at-r1-opus.md §2 (decode rule, proof and differential); base-sdk `contrib/vectors/bls_wrapper/` (generator) |
| Probes | `/work/scratch/bs11a/probe/relic_probe.cpp`, linked against Core's own `librelic`/`libdashbls` (results P1–P5 below) |
| Revision | **rev1** (2026-10-14): the manager's rulings on r0 e1b0e1313. One BLS oracle entry (BS12's, ABI 14; BS11a extends it at ABI 15). The DEC-79 serial verify is BS12's; BS11a-4 adds the engine branch behind it. A-3/A-4 applied in this branch's docs commit. New quirk 45. Scan (h) folded into slice 4 as non-blocking. New D22–D25; D10, D13 and D21 revised |
| Revision | **rev2** (2026-10-14): Sol r1 (CHANGES-REQUIRED) applied. The basic-flag claim is scoped to the three direct verify entry points, and aggregate-then-verify order is stated (F7, S6, D26). D8 is observed by raw-relic mirror ops before dashbls clears the code; the post-wrapper read becomes a state-leak check (§3.3, D28). Lemma T makes the Group → relic transition explicit (D27). R1 is split into G1 and G2. The call-site inventory and COVERAGE/A1 ownership rows are completed. New D26–D29; D4, D8 and D16 revised |
| Revision | **rev3** (2026-10-14): Sol r2 (CHANGES-REQUIRED) applied with the manager's rulings. Core's relic representation is part of every Group value: an exact coord tag plus the infinity shape, coordinates fixed up to an unobservable rescaling (Lemma T′, D27). EQ is in the API and the scripts, and the non-normalizing observables are audited (§3.1, D30). D8 traces each wrapper op raw from pre-call snapshots (D28). Lemma T′ states the G2 mixed/general rescaling. "No arithmetic on the Exotic operand" (D31). New D30–D32; D8, D27 and D28 revised |
| Revision | **rev4** (2026-10-14): Sol r3 (CHANGES-REQUIRED) applied. `NEG` of an infinity resets it to `BASIC` (0, 0, 0) in both groups; the producer table, the `Group::neg` rule, the model target and the transition scripts say so, with Sol r3's vector (D33). The raw traces are re-derived line by line in Core's exact order, including element constructors, the message map with its own relic calls and its `assert`, and every clear point (D34). Trace order is checked against the cited lines and against the real wrapper's call log, not only by the final value (D35). The sweep (§3.5) also corrects the legacy `VERIFY` and `VERIFY_AGG` orders and F1, and adds F9, the basic scheme's distinct-hash rule (D36), and DH's missing `CheckRelicErrors` (D37). New D33–D39; F1, D8 and D28 revised |
| Revision | **rev5** (2026-10-14): Sol r4 (CHANGES-REQUIRED) applied, narrow. `AGG` splits into pair and span paths with the wrapper constructors, and constructors whose presence depends on copy elision are pinned per build (D40, D43). A step is an undefined external relic symbol of the object that makes the call. Static or inlined dashbls helpers are expanded, so `MapL` lists `ep2_sw_encode`'s and `ep2_mul_cof_b12`'s relic calls with their branches, and checker (c) verifies every step symbol with `nm` (D41, D42). The legacy map's once-per-process constants are forced at shim start (D44). `HashPubKeys`' `md_map_sh256` steps were added (D45). The negative-scalar claim (D46) and F8 (D47) are narrowed. New D40–D48; D35 revised |
| Revision | **rev6** (2026-10-14): Sol r5 (CHANGES-REQUIRED) applied, narrow. `SER` keeps `GetHash`, and the wrapper's hash cache is modelled: an uncached valid `SER` makes two `ep*_write_bin` calls, a cached one makes one, and every register carries a cache flag (F10, D49, D50). The build evidence now comes from the pinned v24 build that `dash-oracle` links, with recorded hashes. Checker (c) reads those exact objects, and the `--wrap` seams sit in the recipe's `ld -r` step, the only place they can reach (§3.6, D51, D52, D56). D43's relocation total is a drift guard, and both span paths run under (b) (D53). F8's throw wording is narrowed (D54). The once-initializers are asserted to have run before any window (D55). New D49–D57; D43, D44 and D35 revised |
| Revision | **rev6.1** (2026-10-14): Sol r6 (APPROVE-WITH-NITS) minor applied. The hash cache follows Core's `cachedHash.IsNull()` sentinel in F10, the `SER` row and §7, and an all-zero digest serializes again (D58) |
| Revision | **rev7** (2026-10-14): Opus-h r1 (CHANGES-REQUIRED) applied with the manager's rulings. D8 is certified from the real wrapper's call log: the code after every call into relic, read without clearing it, over a wrap set derived with `nm`, plus the post-op read and a static throw-site table. The mirror trace and the checks that only validated it are superseded, and the step lists stay as non-normative documentation (§3.3, D59, D60, D65). Invalid wrappers are modelled as Core's default element on the ungated paths (F11, D62). ABI 14 is already at the pin, so §7 maps onto its ops and flags, and BS11a-2 starts now (D61). Fold order is a stated contract (D63). Legacy-flag verify gets gated rows against dashd (D64). There is one dispatch rule (D66), and the IES key fix is in the BSX catalogue (D67). New D59–D68; D4, D8 and D13 revised; D25, D28, D34, D35, D40–D45, D49, D50, D52, D53, D55 and D56 superseded in whole or in part |
| Revision | **rev7.1** (2026-10-14): Sol r7 (CHANGES-REQUIRED) applied, narrow; it confirms that D59 keeps coverage at the pin. An invalid wrapper keeps an element of its own: the default element after construction, `Reset` or a failed decode, and the receiver's previous element after a failed in-place share, recovery or DH. So `BlsValue` carries validity and the element apart, the ungated paths read the element, and ABI 15's `INPLACE` flag lets the oracle seed every class under both flags (F11, §6–§8, D69). The wrap set is 84 symbols raw and 83 effective (D70). dashbls's own `RLC_THROW` sites join the static table (D71). New D69–D72; D60, D62 and D65 revised |
| Revision | **rev7.2** (2026-10-14): Sol r8 (CHANGES-REQUIRED) applied, narrow. Op 20 `EQ` is Core's wrapper `operator==` (`fValid`, then `impl`), and the element-only comparison that `BlsElem::eq` ports gets op 26 `IMPL_EQ`; each script names the one it uses (§7, D73). F11 states a throwing `Unserialize`'s result by validity (D74) and `GetHash` as F10 does (D75). New D73–D75; D30, D61 and D66 revised |
| Revision | **rev7.3** (2026-10-16): BS11a-2 Sol r1 nit 5, docs only. §3.3's static throw-site table follows the committed script at the BS11a-2 pins: 870 live relic sites, 187 reachable with gcc and 179 with clang, the three `#if`-dead sites out, and the four macro-body sites `md_xmd.c:48, 68` and `relic_tmpl_map.h:231, 310` in; indirect calls are audited (`indirect.tsv`) |

## 0. Summary

1. **Decode: no relic port** (D1). Legacy decode is blst uncompress plus BS11a-t's proven predicate. The canonical bytes fix relic's internal point exactly, so nothing about decode needs relic code. BS11a changes only *when* Usable vs Exotic is decided: lazily, so that decode stays as fast as today's (D2).
2. **Exotic points do reach relic arithmetic, and A1's "Exotic → `false`" holds only for the three direct verify entry points under the basic flag** (§2, D16, D26).
   - Under the **basic** verify flag, the direct entry points (`VerifyInsecure`, `VerifyInsecureAggregated`, `VerifySecureAggregated`) do no arithmetic on an Exotic operand beyond the exact validity test. `CoreMPL` checks `IsValid()`, or re-decodes and throws, before using it, so the answer is `false` (D31).
   - **Aggregation comes before that gate** where Core aggregates first (the batch verifiers, S6). `AggregateInsecure` runs relic on Exotic inputs under either flag with no validity gate, and only the aggregate reaches the basic gate (F7). Its span form, like `VerifySecureAggregated`, uses an invalid input's `impl`, the element that wrapper still holds (F11).
   - Under the **legacy** verify flag, `LegacySchemeMPL` runs relic's aggregation and pairing on Exotic operands with no checks.
   - **IES DH** multiplies the recipient key with relic under either flag.

   relic's arithmetic on these points is not group arithmetic (P2, P5), so BS11a carries a **transliteration of the reachable relic functions** (the "engine", §3). Values carry their representation, and every operation with a relic-represented operand goes through the engine; all-Usable operations stay on blst (§3.1, D5, D66).
3. **Secret keys:** BS11a-t's rules stand. Legacy (`modOrder`) is canonical ⇔ 0 < v < r; basic ⇔ 0 < v ≤ r, with r kept unreduced. Scalars are reduced mod r before multiplication, as `g1_mul` does (§4, D12).
4. **IES:** Core derives the AES key from the **basic** encoding of `sk·pk` under every flag and scheme. dash-pkc's Chia arm uses the legacy encoding, which is wrong (§5, D11).
5. **API:** Core-shaped, serial functions on `BlsValue` in `dash_pkc::bls::core`: three verifies, each with an explicit flag, and the IES key. The pairing check over Usable operands is BS12's DEC-79 serial verify; BS11a-4 adds the engine branch behind the same function. The engine is private (§6, D17, D20, D22).
6. **Oracle:** no entry of its own. BS11a-2 extends the multi-op BLS script entry already at the pin (`oracle_bls_exec`, ABI 14) at **ABI 15**: new op codes 20–25, flag bits for the call log and the pair form, and relic's error code in the record's reserved field (§7, D61).
7. **Verification:** four targets (`diff_bls_decode`, `diff_bls_arith`, `diff_bls_verify`, the model target `bls_engine_model`) certified per DEC-184/186, and one bench row (§8).
8. **Slices:** BS11a-1 (decode closure) unblocks EV1-2 without waiting for the engine. Then BS11a-2 (oracle ops; ABI 14 is at the pin, so it can start now), BS11a-3 (engine groups and IES) and BS11a-4 (pairing, verify, and scan (h) as a non-blocking item) (§9).

## 1. Decode (PLAN's "relic port")

The PLAN row asks for a port of `ep_read_bin`/`ep2_read_bin`/`fp_srt`. That port is **not needed** (D1):
- BS11at-r1 §2 proves, case by case, that relic's legacy decode followed by Core's round trip is canonical exactly under BS11a-t's predicate. The relic facts it rests on: `CHECK` is off; `fp_read_bin` leaves the output untouched on failure; `ep_upk`'s result is ignored; `ep*_pck` sign rules.
- 305,176 boundary, random and torsion inputs agree with Core's real `CBLSWrapper`, at both pins.
- BS11at-r1 §2.8 shows the canonical bytes determine relic's internal point (Lemma C below), so the engine starts from the bytes, not from a decode port.

**Lemma C (the point behind a canonical encoding).** A valid legacy object with canonical basic bytes `b` holds the relic point `(x, y, 1)` in `BASIC` coordinates, where:
- on the curve: (x, y) is blst's `uncompress(b)`. relic's `ep_upk`/`ep2_upk` sign rule equals blst's for every on-curve point (BS11at-r1 §2.5).
- G1 off the curve: y = s·2⁻³⁸⁴ mod p, where s is the legacy sign bit. relic stores the raw Montgomery digit s.
- G2 off the curve: y = (0, 0), since only s = 0 is canonical.

BS11a's decode changes:

| Change | Why | D |
|---|---|---|
| Legacy decode stops running the subgroup check. `BlsPoint` gets three states: `Usable(A)` (basic-decoded, subgroup known), `OnCurve(A)` (legacy, not classified yet) and `OffCurve`. `usable()` runs `in_g1`/`in_g2` on demand for `OnCurve` | Today's Chia decode is uncompress only. An eager subgroup check costs about one to two extra uncompress times, which would fail the bench gate | D2 |
| Equality and `Hash` ignore the classification state: elements compare by basic bytes plus point, as now | Core compares elements. Without this, the same point decoded under the two schemes would compare unequal | D2 |
| No coordinates are stored for `OffCurve`. The engine rebuilds them from `Valid.basic` (Lemma C) | Keeps `BlsPoint: Copy + Eq + Hash`; Exotic operations are rare | D3 |
| A1 §1.4.1 gets BS11at-r1 M4 (a)–(c): the rule replaces "port", plus the row "G2 legacy, sign set, on the curve with y.c1 = 0: THROW under both flags" | Table completeness (quirk 35) | D24 |

## 2. Where Core computes with Exotic points

### 2.1 The dashbls facts that decide it

| # | Fact (v24 = v23.1.8) | Where |
|---|---|---|
| F1 | `CoreMPL::Verify` and `CoreMPL::AggregateVerify` (basic flag) return `false` when any key or the signature is not `IsValid()` (`g1_is_valid`: on the curve and in the subgroup). Before and between these checks Core only maps message hashes and negates the generator, and neither touches a key or the signature. `Verify` maps the hash first, then negates the generator, then checks the key, then the signature. `AggregateVerify` negates the generator, checks the signature, then for each key checks it and maps its hash, in that order | `dashbls/src/schemes.cpp:135-153`, `:297-324`; `elements.cpp:334-347` |
| F2 | `CoreMPL::VerifySecure` serializes each key under the flag, sorts, hashes (`HashPubKeys`), and then, **one key at a time in sorted order**, re-decodes it with `G1Element::FromBytes(…, fLegacy)`, multiplies it by its weight and adds it to the accumulator. So arithmetic on earlier Usable keys can run before an Exotic key is reached, but no arithmetic touches the Exotic key. Under basic, its re-decode throws (`CheckValid` or relic's error), and Core's wrapper catches it and returns `false` | `schemes.cpp:32-58`, `:231-254`; `bls/bls.cpp:341-358` |
| F3 | `LegacySchemeMPL::Verify` and `::AggregateVerify` **skip `IsValid`** and call `NativeVerify` (relic's pairing) directly. `VerifySecure(legacy)` re-decodes with no checks, computes Σ tᵢ·Pᵢ with relic's `g1_mul`/`g1_add`, and then calls the legacy `AggregateVerify` (virtual dispatch) | `schemes.cpp:658-703`, `:677-681`, `:246-253` |
| F4 | `CBLSPublicKey::DHKeyExchange` = `g1_mul(pk, sk.keydata)` under either flag; the IES key is `ToByteVector(false)`, truncated to 32 bytes | `bls/bls.cpp:208-220`; `dashbls/src/privatekey.cpp:184-190`; `bls/bls_ies.cpp:43-47, 79-83, 96-100` |
| F5 | `CoreMPL::Aggregate` (G1 and G2, both flags) adds with relic and no checks | `schemes.cpp:176-192`; `bls.cpp:144-177`, `:222-255` |
| F6 | `NativeVerify` returns `false` when relic's thread-local error code is set, and then clears it. dashbls `G1Element::IsValid()`/`G2Element::IsValid()` return **true for infinity** | `schemes.cpp:326-350`; `elements.cpp:135-142`, `:358-365` |
| F7 | **Aggregate, then verify.** `CBLSPublicKey/CBLSSignature::AggregateInsecure` aggregate with relic (F5) under the global flag. The pair form asserts both wrappers' `fValid`, which Exotic wrappers pass; the span form tests no `fValid` at all (F11). The batch paths aggregate first and hand only the aggregate to `VerifyInsecureAggregated`: `bls_batchverifier.h` aggregates keys per message hash and all signatures; `net_dkg.cpp` and `bls_worker.cpp` aggregate signatures. So under the basic flag F1 tests the **aggregate**, not the inputs | `bls/bls.cpp:144-177`, `:222-255`; `bls/bls_batchverifier.h:146-178`, `:195-228`; `llmq/net_dkg.cpp:262-283`; `bls/bls_worker.cpp:850-863` |
| F8 | The dashbls operations that compute a new element (`Generator`, `Negate`, `operator+`/`+=`, `operator*(G, bn_t)`, `FromMessage`, and `FromBytes` through `CheckRelicErrors(!fLegacy)` and `CheckValid`) end with `BLS::CheckRelicErrors`, which clears a nonzero code before it returns, or before it throws on a relic error. `CheckValid` throws on `!IsValid()` *before* its clear point (`elements.cpp:144-148, 367-371`), so that path does not clear (D54). The predicates and serializers `IsValid`, `SerializeToArray` and `operator==` have none. Neither do the copies `ToNative`/`FromNative`, nor the secret-key products `operator*(G1Element, PrivateKey)` (which `DHKeyExchange` uses, D37) and `operator*(G2Element, PrivateKey)` (D47) | `dashbls/src/bls.cpp:81-91`; `elements.cpp:24-30, 32-92, 127-133, 154-159, 212-233, 334-347, 385-390, 450-471` (with); `:135-148, 178-203, 358-365, 400-441` (without); `privatekey.cpp:184-205` |
| F9 | `BasicSchemeMPL::AggregateVerify`, which the basic flag's `VerifyInsecureAggregated` reaches, returns `false` when two message hashes are equal. It checks this after the argument check and before any relic call of `CoreMPL::AggregateVerify`. `LegacySchemeMPL::AggregateVerify` has no such check. `VerifySecure`'s inner call has one message, so the check always passes there | `schemes.cpp:416-431` (also `:399-414`), `:683-703`, `:253` |
| F10 | `CBLSWrapper::GetHash` caches `SerializeHash(*this)` in `cachedHash`, and every call tests `cachedHash.IsNull()`; there is no separate flag. When the value is null, it serializes under the **global** flag at that moment: `Serialize(s)` → `Serialize(s, bls_legacy_scheme)` → `ToBytes` → `SerializeToArray` → `g*_write_bin` (nothing for an invalid wrapper). Then it stores the digest. When the value is not null, it returns it under any flag, with no relic call. An all-zero digest stores null, so the next call serializes again (D58). Writes that clear the cache: `AggregateInsecure` (pair `bls.cpp:152`, `:230`; span `ret` `:175`, `:253`), `DHKeyExchange` (`:211`, `:218`), `SubInsecure` (`:293`), and `SetBytes`/`Reset` (`bls/bls.h:95-123`) | `bls/bls.h:141-146`, `:170-180`; `bls.cpp` as listed |
| F11 | **Invalid wrappers keep an element.** A `CBLSWrapper` holds `fValid` and `impl` apart, and an invalid wrapper's `impl` depends on how it became invalid. **The default element** (`set_infty`, `BASIC` (0, 0, 0); zero for a secret key): a default-constructed wrapper; `Reset`; every failed `SetBytes` or `SetHexStr` (wrong length, all zero, a `FromBytes` throw, a default result); an `Unserialize` that accepts the bytes only in the other scheme (it calls `Reset`); a lazy `Get` that fails (it returns the static `invalidObj`) and a lazy copy whose object is not initialized (`obj.Reset()`); the `{}` returns of span `AggregateInsecure` (empty span), `AggregateSecure` (empty or unequal spans) and `GetPublicKey`/`Sign` on an invalid key; and a dashbls throw in span `AggregateInsecure`, `AggregateSecure` or `Sign`, which leaves the fresh result wrapper untouched. **The receiver's previous `impl`:** `SecretKeyShare`, `PublicKeyShare`, `Recover` and `DHKeyExchange` clear `fValid` first and assign `impl` only on success, so every `false` return keeps `impl` as it was. That covers an invalid id or input, empty or unequal spans (`Recover`), and a dashbls throw, `Threshold`'s fewer-than-two, zero-id and duplicate-id throws included. The pair `AggregateInsecure(o)` (G1, G2) sets `fValid = false` on a dashbls throw and keeps its pre-call `impl`. A copy copies both fields; a move swaps both with the target. **Readers:** `ToBytes`, `ToByteVector` and `Serialize` give zero bytes for any invalid wrapper, and `GetHash` is `SerializeHash` over those zero bytes, cached under the null sentinel (F10). The wrapper `operator==` compares `fValid`, then `impl`, and compares `impl` only when the flags agree (`&&`). Span `AggregateInsecure` (secret key, key, signature), `AggregateSecure` and `VerifySecureAggregated` copy each input's `impl` with no `fValid` test, so an invalid input contributes the element it holds. `VerifySecureAggregated` serializes, hashes and re-decodes an invalid key like any other (F2). `VerifyInsecure`, `VerifyInsecureAggregated`, `GetPublicKey`, `Sign`, the share, recovery and DH inputs and the pair `AggregateInsecure` (an `assert`) test `fValid` first. **A throwing `Unserialize`** keeps the second `SetBytes`'s result, so its validity depends on the bytes: valid with that element when the other scheme decodes them (the round trip then fails), invalid with the default element when it rejects them (`SetBytes` calls `Reset`). A stream read that throws before `SetBytes` leaves the wrapper as it was (D74). **Failures that leave a wrapper valid:** `SubInsecure` and the secret-key pair `AggregateInsecure` propagate a throw with both fields unchanged; a lazy `Get` whose round trip fails leaves its inner object valid but unreachable, since it is never returned, copied or serialized (D69) | `bls/bls.h:50-123`, `:149-162`, `:183-203`, `:419-437`, `:492-512`; `bls.cpp:36-60`, `:83-142`, `:144-287`, `:341-358`, `:360-391`; `threshold.cpp:174, 196, 199, 223, 231`; `elements.hpp:39-40, 82-83` |

### 2.2 The relic facts that make Exotic arithmetic non-standard

| # | Fact | Where / evidence |
|---|---|---|
| R1a | **G1:** `ep_add_projc` (mixed when the second operand is `BASIC`, general otherwise) and `ep_dbl_projc` are the complete homogeneous-projective formulas for a = 0 with b3 = 3b built in; apart from the infinity tests at entry they have no branches. On the curve they are the group law. Off the curve they are not a group law, so the result depends on the exact sequence of operations | `relic_ep_add.c:126-445`, `:711-770`; `relic_ep_dbl.c:131-270` |
| R1b | **G2:** `ep2_add_projc` is **Jacobian** addition (relic's `PROJC` tag means Jacobian here), mixed when the second operand is `BASIC`, with explicit branches: H = 0 ∧ R = 0 → `ep2_dbl_projc`; H = 0 ∧ R ≠ 0 → infinity. `ep2_dbl_projc` is a separate Jacobian doubling formula. These are not complete formulas; the branches are part of what is ported | `relic_ep2_add.c:147-183`, `:259-301`, `:390-407`; `relic_ep2_dbl.c:119-157` |
| R2 | `g1_mul`/`g2_mul`: a scalar of at most 64 bits takes `ep*_mul_dig`. Otherwise it is reduced mod r, then multiplied by `ep*_mul_lwnaf`: GLV (`bn_rec_glv`, ψ) in G1 and 4-dimensional GLS (`bn_rec_frb`, Frobenius) in G2, over a w = 4 table from `ep*_tab` normalized with `ep*_norm_sim`. Off the subgroup, k mod r and the endomorphism decomposition do not give [k]P | `relic_pc_exp.c:38-118`, `relic_ep_mul.c:42-151, 673-688`, `relic_ep2_mul.c:43-130, 334-354` |
| R3 | `fp_inv(0) = 0` with no error code: GMP `mpn_gcdext` with U = 0 gives S = 0. `fp_inv_sim` (batch inversion) then zeroes **every** entry when one z is 0, and `ep_norm_sim` turns those entries into the affine point (0, 0), which is not infinity | `low/gmp/relic_fp_inv_low.c:42-69`, `relic_fp_inv.c:523-560`, `relic_ep_norm.c`; **P1** |
| R4 | The pairing `pp_map_sim_oatep_k12`: pairs with an infinity side are dropped, the rest normalized. `pp_mil_k12` runs over the NAF of \|x\| and evaluates lines at (3x_P, −y_P) (`pp_dbl_k12`/`pp_add_k12`, projective LAZYR), then conjugates for x < 0 and calls `pp_exp_k12` (Ghammam–Fouotsa). For off-curve operands the Miller value depends on this chain, and blst uses a different addition chain | `relic_pp_map_k12.c:51-125, 523-575`, `relic_pp_exp_k12.c:134-` |

**Probe** (Core's own relic build, `relic_probe.cpp`):
- **P1:** `fp_inv(0) = 0`, and the error code stays `RLC_OK`.
- **P2:** `g1_mul` of the order-3 point (0, 2), which is legacy-canonical and Exotic, by a 255-bit scalar returns the affine point (0, 0). That is neither infinity nor the true multiple (R2, R3).
- **P3:** `g1_mul` and `g1_add` on an off-curve G1 point set no error code.
- **P4:** `pp_exp_k12(0) = 0`.
- **P5:** a zero Miller value is reachable from an Exotic G2 operand (off curve, y = 0). No error code is set.

So **arithmetic on Exotic points needs relic's own algorithms**: neither blst nor "the mathematically right answer" reproduces Core.

### 2.3 Call-site inventory (v24; v23.1.8 has the same sites)

"Flag" is the global `bls_legacy_scheme`: legacy before V19, basic after.

| # | Site | Operands that can be Exotic | Basic flag | Legacy flag | Needed by |
|---|---|---|---|---|---|
| S1 | `CheckHashSig` (ProUpServ/ProUpRev/ProUpReg sig vs operator key), `evo/specialtxman.cpp:1088-1094`, `VerifyInsecure(pk, h)` under the global flag | the key (v1, lazily decoded legacy; accepted until V24, `specialtxman.cpp:1158-1160`) and the sig (v1) | F1: `false` | F3: relic pairing | EV1-4 |
| S2 | `CFinalCommitment::Verify` `membersSig`, `llmq/commitment.cpp:75-93`; `VerifySecureAggregated` has no validity gate (F11). The queued form is `utils::BlsCheck` (`llmq/utils.cpp:558-577`), which uses `VerifyInsecure` when exactly one key | member operator keys | F2: `false` (F1 with one key) | F3: relic Σ tᵢ·Pᵢ + pairing | CH7b, LQ2 |
| S3 | single-member LLMQ, `commitment.cpp:70-73` | the member key | F1 | F3 | CH7b |
| S4 | `quorumSig` vs `quorumPublicKey`, `commitment.cpp:104` | v1/v2 commitment fields (legacy by version) | F1 | F3 | CH7b |
| S5 | CLSIG, ISDLOCK, recsig, MNHF, asset unlock, platform ban vs a quorum key (`llmq/quorumsman.cpp:796`, `evo/mnhftx.cpp:152`, `evo/assetlocktx.cpp:148`, `net_processing.cpp:3904`, `signing_shares.cpp:739`) | quorum key (v1/v2), sig (decoded under the flag) | F1 | F3 | LQ3, CH7b |
| S6 | Aggregate-then-verify (F7): `bls_batchverifier.h:146-228` (keys aggregated per message hash, all sigs aggregated, then `VerifyInsecureAggregated`; per-message fallback); `net_dkg.cpp:262-283` and `bls_worker.cpp:850-863` (sigs aggregated) | keys and sigs | F5 on the inputs (no gate; an invalid input contributes its `impl`, F11), then F1 on the aggregate | F5, then F3 | LQ3, LQ4a |
| S7 | DKG message sigs vs member keys, `llmq/net_dkg.cpp:217-320`; the premature commitment's `quorumSig` vs a public-key share built from the quorum vvec, `llmq/dkgsession.cpp:603-609` (S9 arithmetic first) | member keys; the derived share | F1 | F3 | LQ4a |
| S8 | DKG contributions: IES encrypt to every member key, `active/dkgsession.cpp:84-95`; decrypt with your own key against the ephemeral key | member key; ephemeral key (decoded under the flag) | F4 | F4 | LQ4a |
| S9 | `PublicKeyShare` over a verification vector, and `Recover` (`bls/bls_worker.cpp:571, 682, 737`, `signing_shares.cpp:723`, `llmq/dkgsession.cpp:603`) | vvec entries and sig shares (decoded under the flag) | F5/R2 | F5/R2, then F3 | BS11b, LQ3, LQ4a |
| S10 | MNAUTH (`evo/mnauth.cpp:98`), governance, CoinJoin, quorum proof certificates (`llmq/quorumproofs.cpp:305-319`), all `VerifyInsecure(…, false)` | operator key; quorum key (proofs) | F1 | (always basic) | P2/GV, LQ3 |
| S11 | `operator==`, `GetHash`, unique-property hashes | any | exact through bytes (BS11a-t, EV1 D17) | same | done |

Everything in S1–S9 is in BS11a's engine scope at the element level. BS11a owns the three verify functions (S1–S7) and the DH key (S8). BS11b composes S6, S8 multi-recipient and S9 on top of the engine (D10).

## 3. The relic engine (Exotic representation and arithmetic)

### 3.1 Dispatch rule (D4, D66)

**One rule: dispatch by representation.** Every element is `Elem::Group(blst point, identity included, plus Core's relic coord tag)` or `Elem::Relic(relic::Point)`.
- **Producers.** Decode gives `Group` for a Usable value and `Relic` for an Exotic one (Lemma C). A wrapper's `impl` is used whether or not the wrapper is valid; the default element is `Group` `BASIC` (0, 0, 0) (F11). `ADD`, `NEG`, `MUL` and `AGG` give `Group` when every operand is `Group`, computed by blst (serial entry points only, D17). Otherwise they give `Relic`, computed by the engine, with each `Group` operand rebuilt per Lemma T′. An engine result stays `Relic` even when it lands in the subgroup, so later operations see exactly relic's coordinates.
- **Element equality** (`BlsElem::eq`, script op `IMPL_EQ`) splits the same way: blst when both sides are `Group`, otherwise the transliterated `ep*_cmp` (D30, D73).
- **Only the pairing converts.** A `Relic` operand that is on the curve and in the subgroup is normalized and verified by blst (D22). If any operand is outside the subgroup, the engine computes the Miller values (Lemma E).

Lemma U justifies the `Group` path and the pairing conversion: on on-curve subgroup points, relic's complete formulas are the group law; any GLV/GLS decomposition of k mod r gives [k]P; and Miller functions with the same divisor agree up to Fp6* constants, which the final exponentiation kills. There are no shortcuts for on-curve Exotic points. They are `Relic` from decode, and even they need relic's recoding (R2) and its zero-z handling (R3).

**Relic shape of a Group value (D27).** Core holds every element as relic coordinates with a coord tag, and two observables do not normalize: `ep_cmp`/`ep2_cmp` (EQ, below). So an `Elem::Group` value carries, besides blst's value, **the coord tag Core's relic would have**, and from it the infinity shape. The tag follows Core's code, op by op; no relic run is needed:

| Producer (Core) | Tag | Finite | Infinity |
|---|---|---|---|
| decode (Lemma C), `Generator()`, every `g*_mul`/`g*_mul_gen` path (each ends in `ep*_norm`, or `set_infty` for k = 0 or an infinite input; a negative scalar of at most 64 bits ends with `g*_neg`, which keeps `BASIC` by the `NEG` rows; a longer negative scalar is first reduced by `bn_mod` to a non-negative one, also `BASIC`; D46) | `BASIC` | (x, y, 1) | (0, 0, 0) |
| `g*_add(r, p, q)`, p infinite. relic tests p first, so with two infinities the result is q | q's | q, copied | q, copied |
| `g*_add(r, p, q)`, p finite, q infinite | p's | p, copied | — |
| G1 `g1_add` otherwise (complete formulas) | `PROJC` | (λx, λy, λ) | (0, y ≠ 0, 0) |
| G2 `g2_add` otherwise (Jacobian; H = 0 ∧ R ≠ 0 writes `set_infty`, then the `PROJC` tag) | `PROJC` | (λ²x, λ³y, λ) | (0, 0, 0) |
| `g*_neg`, finite input | unchanged | x, z copied, y negated | — |
| `g*_neg`, infinite input (`ep*_is_infty` → `ep*_set_infty`) | **`BASIC`**, whatever the input's tag | — | (0, 0, 0) |

Sources: `elements.hpp:39-40, 82-83` (the default element is `set_infty`), `elements.cpp:94-99, 150-159, 212-231, 319-324, 385-390, 450-470`; `relic_ep_add.c:742-754`; `relic_ep2_add.c:174-183, 210, 333, 390-407`; `relic_ep_neg.c:38-52`; `relic_ep2_neg.c:39-52`; `relic_ep_util.c:39-48` (`set_infty` writes (0, 0, 0) and `BASIC`); `relic_ep2_util.c:39-48`; `relic_pc_exp.c:38-66`; `relic_ep_mul.c:101, 129, 752, 777`; `relic_ep2_mul.c:81, 96, 395, 420`; `relic_ep_mul_fix.c:180`. Probe: `/work/scratch/bs11a/probe/shape_probe.cpp` printed every row against Core's relic. rev4 added `NEG` of both infinity shapes, the generator and a decoded point in both groups, and a check of the `NEG` rule against Core over the probe's Relic set (0 mismatches).

**Implementation rule (D27, D33).** `Group::add(p, q)`: if p is infinite, take q's tag and shape; else if q is infinite, take p's; otherwise the tag is `PROJC`. `Group::neg`: an infinite input gives `BASIC` (0, 0, 0); a finite input keeps its tag. `Group::mul`, decode and the generator give `BASIC`. `ToNative`/`FromNative` copy the tag. `bls_engine_model` checks every rule against the engine's own run of the same op sequence.

**Lemma T′ (rescaling is unobservable, D27).** Take two relic representations with **the same coord tag** that differ by a nonzero rescaling: homogeneous for G1, (λx, λy, λz); Jacobian for G2, (λ²x, λ³y, λz); for a `BASIC` representation, λ = 1. Every ported function gives outputs that again differ only by a rescaling, with equal tags and equal branch decisions. The reasons:
- (a) the formulas are polynomials, homogeneous (G1) or weighted-homogeneous (G2) in each point argument;
- (b) every branch tests either the coord tag, which is equal, or a zero pattern, which rescaling keeps: z = 0, and G2's H = 0 and R = 0;
- (c) mixed and general addition agree up to a rescaling. For G1 the mixed formula is the general one at z₂ = 1, so λ = 1. For G2 the general formula at z₂ = 1 equals the mixed one rescaled by **λ = 2** (x·4, y·8, z·2): the general path doubles R, uses I = (2H)² and J = H·I, and sets z₃ = 2z₁z₂H, while the mixed path has z₃ = z₁H. Since (a) and (b) hold for both, the choice between them is unobservable;
- (d) `ep_cmp` and `ep2_cmp`'s cross-multiplications are bilinear in the two arguments' scales, and normalizing observables (`ep*_norm`, `ep*_norm_sim`, `ep*_write_bin`, `ep*_on_curve`, `g*_is_valid`, the pairing's input normalization) remove the scale.

Rescaling cannot change the infinity *shape*, since (0, y ≠ 0, 0) and (0, 0, 0) differ by a zero pattern, not by a scale. That is why the shape is carried exactly. (a)–(d) never use the curve equation, so T′ also covers off-curve operands and engine results fed back. **Consequence:** when a Group value meets a Relic value (an op or EQ), the engine rebuilds it as follows, and the result is observationally Core's:
- tag `BASIC`: (x, y, 1), or (0, 0, 0);
- tag `PROJC`, finite: (x, y, 1);
- tag `PROJC`, infinite: (0, 1, 0) in G1, (0, 0, 0) in G2.

The shape probe checks this: rebuilt operands with a random λ gave the same `g*_cmp` and normalized `g*_add` results as Core's own, in both operand orders. The Relic side was the P2 artifact, the order-3 point, an off-curve point, and engine results including a zero-z G2 double. Results: G1 200×9×10, G2 100×8×7, 0 mismatches.

**EQ and the other non-normalizing observables (D30).** The audit of every relic observable that the wrappers reach:

| Observable | Normalizes? | Depends on | Covered by |
|---|---|---|---|
| `ep_cmp` (`G1Element::operator==`, `elements.cpp:200-203`) | never; cross-multiplies by tag (`relic_ep_cmp.c:38-92`) | tags, infinity shape, affine values | shape + T′ |
| `ep2_cmp` (`elements.cpp:440`) | only if a tag is `BASIC`; else Jacobian cross-multiplication (`relic_ep2_cmp.c:40-80`) | tags, infinity shape | shape + T′ |
| `g*_is_valid` (internal `ep*_cmp` on Bowe/Scott intermediates) | `on_curve` normalizes; the cmp does not | intermediates computed from the input, whose tags follow the input's | shape + T′ |
| `ep*_is_infty`, dashbls `IsValid`'s infinity test | z = 0 only | zero pattern | T′ (b) |
| `ep*_norm_sim` inside `ep*_tab` | its zero-z batch rule (R3) | zero pattern | T′ (b) |
| `ep*_write_bin`/`SerializeToArray`, `ep*_on_curve`, `pp_map_sim` inputs, `gt_cmp`/`fp12` values | yes, or no point coordinates | — | — |

**Group-vs-Group EQ is group equality.** Finite representations cross-multiply to affine equality. Finite vs infinite gives NE: the G1 shape (0, y ≠ 0, 0) cross-multiplies to NE; `set_infty` (0, 0, 0) is EQ only to a point with affine (0, 0), and no curve point has that affine; G2 normalizes or cross-multiplies the same way. Two infinities are EQ. So `BlsElem::eq` uses blst when both sides are Group, and the transliterated `ep*_cmp` on rebuilt representations otherwise.

Sol's fixed vector is a §8 seed: `g1_cmp(G + (−G), [k]·(0, 2)) = NE` but `g1_cmp(set_infty, [k]·(0, 2)) = EQ`. The second form is the r2 rebuild this design no longer uses. Sol r3's vector is a seed too: `g1_cmp(NEG(G + (−G)), [k]·(0, 2)) = EQ`, because `NEG` turns the `PROJC` infinity into `BASIC` (0, 0, 0). The rev4 probe reproduced both answers (`cmp` = 0 with the `NEG`, 2 without).

### 3.2 What is transliterated (D5)

The engine is a line-by-line Rust transliteration of these relic functions, as configured (front-matter table). Each function's source span is cited in its doc comment, and Core's relic is the pin:

| Layer | relic functions | Notes |
|---|---|---|
| Field tower (D6) | `fp_*`, `fp2_*`, `fp6_*`, `fp12_*` that the functions below call: `mul`, `sqr`, `add`, `sub`, `neg`, `dbl`, `inv`, `mul_nor`, `mul_dxs`, `conj`/`inv_cyc`, `frb` | Exact maths over Fp[u]/(u²+1) → [v]/(v³−(1+u)) → [w]/(w²−v), the same tower as blst. **inv(0) = 0 at every level** (R3, P1). Implemented over blst's serial `blst_fp*`, `blst_fp2*` and `blst_fp12*` C functions (Montgomery, R = 2³⁸⁴ as relic), with the zero branch written out |
| Integer recoding | `bn_mod`, `bn_rec_naf` (w = 2, 4), `bn_rec_glv` with relic's `v1`/`v2` lattice, `bn_rec_frb` (BLS12 flag) | Pure Rust, fixed-width signed integers, no allocation. Rounding exactly as relic's `bn_div`/`bn_rsh` |
| G1 | `ep_add_projc` (mixed + general, complete, a = 0; R1a), `ep_dbl_projc`, `ep_neg`, `ep_sub`, `ep_psi` (β as relic sets it), `ep_norm`, `ep_norm_sim`, `ep_tab`, `ep_mul_glv_imp`, `ep_mul_lwnaf`, `ep_mul_dig`, `g1_mul`, `ep_on_curve`, `ep_cmp`, `ep_write_bin`/`ep_pck` | Including relic's coordinate tags (`BASIC`/`PROJC`) and infinity tests (z = 0) |
| G2 | the `ep2_` counterparts, plus `ep2_frb` and `ep2_mul_glv_imp`'s four-way table | Jacobian `PROJC`, with the H = 0 / R = 0 branches of R1b; no G1 formula assumption carries over |
| Pairing (D7) | `pp_map_sim_oatep_k12` (B12 branch), `pp_mil_k12`, `pp_dbl_k12_projc_lazyr`, `pp_add_k12_projc_lazyr` | Ends at the Miller value f; no final exponentiation (below) |
| dashbls glue | `SerializeToArray` (both schemes, infinity → `c0…`), `FromBytes` re-decode in `VerifySecure` (Lemma C), `HashPubKeys` (sort, SHA-256, `bn_mod_basic`), `NativeVerify` (chunks of 250, the `IsValid` gates of F1, `VerifyAggregateSignatureArguments`), `IsValid` (∞ → true) | Ported from `schemes.cpp`/`elements.cpp`. Also used on the all-Usable path (D9) |

**Not transliterated:**
- decode (§1);
- hash-to-G2 (`ep2_map_legacy` and the basic ciphersuite always give subgroup points; dash-pkc's `chia_h2c` and BS12's vectors cover them);
- subgroup and curve tests (exact predicates: on the curve ∧ in the subgroup, computed on the normalized point). relic's Bowe/Scott tests (`relic_pc_util.c`) are exact on every torsion part:
  - G1: the operator (z² − 1)(ψ + 1) + 1 is ≡ 1 mod every ℓ | h₁ = 3·11²·10177²·859267²·52437899², because each such ℓ divides z − 1. So it is invertible on every ℓ-power part.
  - G2: with ψ² − tψ + p = 0 (t = z + 1), Res(x² − tx + p, z·x³ − x² + 1) mod ℓ is 5, 9, 2056, 9841 and 127701 for ℓ = 13, 23, 2713, 11953 and 262069, and non-zero for the 135-digit cofactor prime of h₂ (checked in `/work/scratch/bs11a/rev7/res.py`, Opus r1);
- the final exponentiation (Lemma E).

**Lemma E (the pairing check).**
- `NativeVerify` accepts iff Π_c FE_relic(f_c) = 1 over chunks c.
- For f ≠ 0, the easy part makes f unitary, and the Ghammam–Fouotsa hard part gives FE_relic(f) = FE(f)^k for a fixed k with gcd(k, r) = 1. FE is the mathematical map f ↦ f^((p¹²−1)/r) (the conjugation for x < 0 maps FE(f) to its inverse).
- All FE values lie in μ_r. So the verdict is: (every f_c ≠ 0) ∧ FE(Π_c f_c) = 1. FE runs on blst's serial `blst_final_exp`, and FE_relic(0) = 0 (P4).
- **Assumption:** relic's final exponentiation is exact on unitary inputs, which every honest Core verification already relies on.
- **Proof obligation (BS11a-4):** a scratch probe compares `pp_exp_k12` with blst on 10⁶ random Fp12 elements (unitary, non-unitary and 0), checking "is 1" agreement and the value of k.

### 3.3 Error state (D8, D59)

relic's error code is per thread and survives across calls. If a reachable path set it for a normal input, Core's result could depend on the thread's history. That cannot be ported, so it would be a finding for the manager.
- **Claim (D8):** for normal, successfully allocated inputs, no relic call on the ported arithmetic, comparison, validity and pairing paths sets the code.
  - **Decode calls are excluded.** `g*_read_bin` can set the code on a non-decodable input. dashbls's `CheckRelicErrors(!fLegacy)` consumes that code as part of the decode outcome, which BS11a-t models (BS11at-r1 §2); this includes the re-decode inside `VerifySecure`.
  - Out of scope: the `ERR_NO_MEMORY` branches (`relic_ep_norm.c:112-119`; `relic_pp_map_k12.c:51-68, 523-534`). An allocation failure is not an input-driven outcome.
  - The `fp2_mul_nor` default error is unreachable for p mod 8 = 3 (`relic_fp2_mul.c:117-150`).
  - Probes P1, P3 and P5 show no code set on the inversion, arithmetic and degenerate-pairing paths.
- **A post-wrapper read cannot certify the claim.** `BLS::CheckRelicErrors` clears a nonzero code before it returns or throws, and every element operation calls it (F8). `NativeVerify` clears it on a failed check (F6).
- **The code is sticky inside relic (D34).** relic never clears its code. `ctx_t`'s first member is `int code` (`relic_core.h:135-137`). With `CHECK` off, `RLC_THROW` only sets it, and with `QUIET` on it prints nothing (`relic_err.h:251-268`); `RLC_CATCH_ANY` is `if (0)` (`:239`). The pinned objects confirm this configuration: `librelic.a` defines `err_get_code` but not the `CHECK`-only `err_get_msg`, and no member references `util_print`. The only resets are `core_init` (`relic_core.c:121`), `err_get_code` (`relic_err.c:101-104`, which dashbls calls only in its per-thread initializer, `bls.cpp:35-45`), `CheckRelicErrors` (`bls.cpp:81-91`) and `NativeVerify`'s failure branch (`schemes.cpp:343-346`). So a read after a call that dashbls or Core code makes into relic sees any code set at any depth inside that call. These are the **depth-0 calls**.
  - **Probe at the pin (Opus r1 M1's two preconditions).** `/work/scratch/bs11a/m1probe/probe.c` links `__wrap_` seams with `ld -r --wrap` against the pinned `librelic.a`. A code set by `ep_read_bin` on a 5-byte input stays set across repeated `core_get()->code` reads in the wrapper and across a later successful `ep_set_infty`, and only `err_get_code` clears it (`log=[0 1 1] reads=1,1 after_ok_call=1 err_get_code=1 after=0`).

**Observation point: the real wrapper's call log (D59, D60).** dashbls and Core's `bls` object are not patched, and nothing re-runs them.
1. **Wrap set (D60).** It is every symbol that is `U` in a dashbls object or in Core's `bls` object and `T` in a relic member, minus `core_get` (the wrappers call the real accessor, and it sets no code). The recipe computes it with `nm` on the `ld -r` inputs at every build. At the pin the raw intersection has 84 symbols and the wrap set, without `core_get`, has 83 (§3.6, D70). A `__wrap_<symbol>` is generated for each from relic's header prototype at the pin, and the recipe turns each into `--wrap=<symbol>` on its `ld -r` line (D56). The build fails when a symbol has no parsable prototype, or when the generated count differs from the wrap set's size (83 at the pin, not the raw 84). That is the only drift guard, and it costs nothing to maintain.
2. **Logging.** Each `__wrap_` calls `__real_`. When the shim's window flag is on and a thread-local depth counter returns to 0, it appends the symbol and `core_get()->code` to the op's log. Reading the field does not clear it. `--wrap` also redirects relic's own cross-object calls, so the depth counter keeps them out of the log (the probe saw 112 such calls during relic's parameter set-up).
3. **Window.** It covers just the real wrapper call. Operand construction (register load, the out-wrapper of `DH`, the `MUL` scalar's `bn_read_bin`, the pair form's copy) runs outside it. The code is `RLC_OK` when the window opens unless an earlier op leaked one, and that op's post-op read already reports the leak. Outside the window, only decode (which dashbls clears) and a 32-byte `bn_read_bin` touch relic.
4. **Certification.** Every log entry outside the decode symbols (`ep_read_bin`, `ep2_read_bin`: the `VERIFY_SEC` re-decode) reads `RLC_OK`, and the post-op read (the record's code field, §7) reads `RLC_OK`. dashbls clears the code right after each decode read (`CheckRelicErrors(!fLegacy)`) before any other wrapped call, so the exemption cannot hide a later set.
5. **Selftest (D59).** A test-only export outside the versioned ABI arms one `__wrap_` to set the code after its n-th windowed call. For every wrap-set symbol that the required scripts reach, the L0 selftest asserts that the log reports the injected code at that entry and keeps reporting it until Core's own clear point.

The once-per-process constants of the legacy map (`std::call_once`, `legacy.cpp:17-41`) may fall inside a window. Their `bn_make` and `bn_read_str` set no code (the throw-site table), so an unwarmed log is harmless.

**Static throw-site table (D65, D71).** D8 is also checked statically, over relic's call graph from the wrap set and over dashbls's own `RLC_THROW` sites, and the log guards it.
- **Method.** The committed script `contrib/node/oracle-relic/throwsites.py`, which `build-oracle.sh` runs on every build of each flavor (rev7.3, BS11a-2), builds a call graph from `objdump -dr` of that flavor's `librelic.a`: call relocations, direct targets and tail jumps. Its roots are the wrap set minus the init-only symbols (`core_get`, `core_init`, `core_set_thread_initializer`, `ep_param_set_any_pairf`, `err_get_code`).
  - An inlined static helper is reachable when a reachable function in its file names it, and every same-file function a reachable body names is followed, to a fixpoint.
  - Every indirect call or jump in a reachable function must have a row in `contrib/node/oracle-relic/indirect.tsv`, whose targets join the graph: the `map_fn` dispatch of `ep*_map_from_field` (SSWU or SvdW), `arch_lzcnt`'s two implementations, and `core_get`'s thread initializer (dashbls's, run by the shim's entry check before any window). An unaudited one fails the check.
  - Sites are the live `RLC_THROW`s of the sources of the archive's members, read through the flavor's own preprocessor (`CC -E -fdirectives-only`): a site in an `#if` branch the pin does not build is no site, which removes `bn/relic_bn_mem.c:69` (`ALLOC = DYNAMIC`), `rand/relic_rand_call.c:81` and `rand/relic_rand_udev.c:62` (`RAND = CALL`/`UDEV`). A site in a macro body belongs to every function using the macro.
- **Result (rev7.3, at the BS11a-2 pins).** 79 roots. Of 870 live relic `RLC_THROW` sites, 187 are reachable in gcc's plain archive (375 functions) and 179 in clang's fuzz archive (372 functions); the 8 fewer are `ERR_CAUGHT` sites whose `if (0)` branches clang folds away. Rev7.2's 913 and 191 counted the 278 sources `Makefile.relic.include` lists, the `#if`-dead sites, and missed four macro-body sites (`md/relic_md_xmd.c:48, 68`, `tmpl/relic_tmpl_map.h:231, 310`), now in the table below.
- **Process.** BS11a-2 commits the script and its tables (`throwsites.tsv`, `indirect.tsv`), and the recipe reruns it on every build. A reachable site outside these classes, or an unaudited indirect call, stops the build.

| Class | Sites (gcc / clang) | Why no normal input sets the code |
|---|---|---|
| `ERR_CAUGHT` | 128 / 120 | all inside `RLC_CATCH_ANY`, which is `if (0)` with `CHECK` off |
| `ERR_NO_MEMORY` | 9 | out of scope (D8) |
| decode | 13: `ep/relic_ep_util.c:243, 249, 266, 276, 282`; `epx/relic_ep2_util.c:252, 258, 276, 286, 292`; `fp/relic_fp_util.c:236, 246`; `fpx/relic_fpx_util.c:77` | `ep*_read_bin`, and the `fp_read_bin`/`fp2_read_bin` it calls: D8's decode exclusion |
| not on an ABI op path | 6: `fpx/relic_fpx_util.c:274, 284, 453, 481, 490`; `rand/relic_rand_hashd.c:159` | `fp6`/`fp12` read and write are `GTElement` serialization, which no op uses. `rand_bytes` is reached through `bn_rand` (key generation) and init seeding, outside every window |
| caller-fixed lengths | 13: `ep_write_bin` (`ep/relic_ep_util.c:296, 311, 319`), `ep2_write_bin` (`epx/relic_ep2_util.c:305, 319, 327`), `fp_write_bin` (`fp/relic_fp_util.c:273`), `fp2_write_bin` (`fpx/relic_fpx_util.c:102, 111`), `bn_write_bin` (`bn/relic_bn_util.c:478`), `bn_rec_naf` (`bn/relic_bn_rec.c:154`), `bn_rec_slw` (`:122`), `md_xmd` (`md/relic_md_xmd.c:68`) | dashbls and relic pass buffers sized for the type; `ep_map_dst` and `ep2_map_dst` ask `md_xmd` for 128 or 256 bytes under the scheme's constant tag |
| caller-fixed preconditions | 14: `bn_make` (`bn/relic_bn_mem.c:46, 81`), `bn_grow` (`:133`), `dv_zero` (`dv/relic_dv_util.c:59`), `bn_get_bit` (`bn/relic_bn_util.c:124`), `bn_div`/`bn_div_rem` (`bn/relic_bn_div.c:137, 145`), `bn_read_str` (`bn/relic_bn_util.c:317`), `ep_map_from_field` (`ep/relic_ep_map.c:93`), `ep2_map_from_field` (`epx/relic_ep2_map.c:106`), `md_map_sh256` (`md/relic_md_sha256.c:47, 51, 55`), `md_xmd`'s SHA-256 calls (`md/relic_md_xmd.c:48`) | sizes are non-negative and at most `RLC_BN_SIZE` (operands are at most 2·381 + 64 bits, well under `BN_PRECI` 1024, `configure.ac:635`); divisors are r or curve constants; bit indices are non-negative; the radix is 16; map lengths are fixed by `ep*_map_dst`; SHA-256 fails only on a null pointer, an input over 2⁶¹ bytes or state misuse |
| unreachable by arithmetic | 4: `fpx/relic_fpx_srt.c:83, 106` (`fp2_srt`, reached from the legacy map's `SwEnc`, `ep2_upk` and the basic map); `tmpl/relic_tmpl_map.h:231` (SSWU), `:310` (SvdW) | p ≡ 3 mod 4, and the non-residue is −1. Line 83 (a₁ = 0, a₀ a non-square): −a₀ is a square. Line 106 (a₁ ≠ 0, norm a₀² + a₁² = s²): ((a₀ + s)/2)·((a₀ − s)/2) = −a₁²/4 is a non-square, so exactly one factor is a square. SSWU: u is a non-square and g(x₂) = u³t⁶·g(x₁), so g(x₁) or g(x₂) is a square. SvdW: for the curve's constants one of g(x₁), g(x₂), g(x₃) is a square |

**dashbls's own sites (D71).** An `RLC_THROW` expanded in a dashbls object sets the code without a call into relic, so the wrap set does not log it there. The script also scans the pinned dashbls sources for the macro:

| Site | Function | Why no normal input sets the code |
|---|---|---|
| `legacy.cpp:80` | `fp_param_get_var` | inside `RLC_CATCH_ANY`, which is `if (0)` with `CHECK` off |
| `legacy.cpp:133` | `ep2_mul_cof_b12` | the same |
| `legacy.cpp:417` | `ep2_map_legacy` | the same |
| `threshold.cpp:67` | `fp_inv_exgcd_bn` | the same |
| `legacy.cpp:336` | `ep2_map_legacy`, `len != RLC_MD_LEN` | every BS11a path hashes a 32-byte `uint256` |

**Step lists (non-normative documentation, D59).** The lists below record Core's depth-0 call order as rev4–rev6 derived it. Nothing compares them with the log or gates on them. A log that differs from them is a documentation bug, not a finding.

**Step lists** (re-derived line by line from v24.0.0-rc.3 for rev4; rev5 applies the symbol rule). `·` separates steps. `[clr]` marks a `CheckRelicErrors` clear point and `stop` is where Core returns `false` or throws; both are annotations, not logged steps. A *ctor* is the element default constructor's `g*_set_infty` (`elements.hpp:39-40, 82-83`), whether it constructs a dashbls element or a `CBLSWrapper`'s `impl` (`bls/bls.h:50, 57`). With ALLOC = AUTO, `bn_new` is `bn_make` and `bn_free`, `ep*_new` and `fp*_new` make no calls. Operand construction (decode, the shim's `bn_read_bin` of a `MUL` scalar, the out-wrapper of `DH`) happens before the logging window in both runs.

**Notation (the rev5 symbol rule, D41).** A step is an **undefined external relic symbol** of the **input object** that makes the call, as the pinned v24 build feeds it to the recipe's `ld -r` (§3.6): `libdashbls_la-elements.o`, `-schemes.o`, `-legacy.o` or `-privatekey.o` in that build's `libdashbls.a`, or Core's `bls` object for the wrapper constructors. Where a source token is a macro, the step is its linked symbol, and rows may show either form (`g1_add` → `ep_add_projc`, `g1_is_infty` → `ep_is_infty`, `g1_get_gen` → `ep_curve_get_gen`, `g2_get_ord` → `ep_curve_get_ord`, `g1_copy` → `ep_copy`, `g1_read_bin` → `ep_read_bin`, `g1_write_bin` → `ep_write_bin`, `g2_cmp` → `ep2_cmp`, `md_map` and `Util::Hash256` → `md_map_sh256`, `fp2_mul` → `fp2_mul_integ`, `fp2_add` → `fp2_add_integ`, `fp12_mul` → `fp12_mul_lazyr`, `gt_cmp` → `fp12_cmp`, `pc_map_sim` → `pp_map_sim_oatep_k12`). A static or inlined dashbls helper is never a step: its body is expanded in place. The block names below (`Gen`, `SwEnc`, …) are notation for their expansions. `core_get` is not a step either: it is a state accessor, the wrappers call the real one, and it cannot set the code. It is excluded from the wrap set, so `[clr]`, `NativeVerify`'s code read and `ep2_curve_get_b`'s context read log nothing.

First the building blocks:

| Block | Steps | Source |
|---|---|---|
| `Gen` = `G1Element::Generator()` | ctor · `g1_get_gen` · [clr] | `elements.cpp:127-133` |
| `Neg(x)` = `Negate()` | ctor · `g*_neg(ans, x)` · [clr] | `elements.cpp:154-159`, `:385-390` |
| `Valid(x)` = dashbls `IsValid()` | `g*_is_infty(x)`: infinite → true; else · `g*_is_valid(x)` | `elements.cpp:135-142`, `:358-365` |
| `Copy(x)` = `ToNative` | `g*_copy` | `elements.cpp:150-152`, `:373-375` |
| `Map(h)`, basic: `G2Element::FromMessage(h, dst)` | ctor · `ep2_map_dst(ans, h, 32, dst)` · [clr] · `Valid(ans)` (the `assert`; Core refuses to build without assertions, `util/check.h:49-51`) | `elements.cpp:334-347` |
| `MapL(h)`, legacy: `FromMessage(h, nullptr, 0, true)` | ctor · `ep2_map_legacy`'s body: `bn_make` ×4 · `md_map_sh256` ×8 · `bn_read_bin` ×4 · `fp_prime_conv` ×4 · `fp_copy` ×4 · `SwEnc(t0)` · `SwEnc(t1)` · `ep2_add_projc(p0, p0, p1)` · `MulCof(p, p0)` · then [clr] · `Valid(ans)` | `elements.cpp:334-347`; `legacy.cpp:306-433` (its `len ≠ 32` throw is unreachable: the wrappers pass 32-byte hashes) |
| `SwEnc(p, t)` = the static `ep2_sw_encode`, expanded. In the pinned v24 build GCC 15 inlines its `t = 0` test (`fp2_is_zero`, `ep2_set_infty`) into `ep2_map_legacy` and outlines the rest as the local `ep2_sw_encode…part.0`. The order of calls is unchanged, and `std::call_once` adds `pthread_once`, which is not relic (§3.6) | `fp2_is_zero(t)`: zero → `ep2_set_infty(p)`, return. Else `bn_make` ×2 · `fp2_neg` · `fp_write_bin` ×2 · `fp2_mul_integ` · `fp_copy` ×2 (`ep2_curve_get_b`) · `fp2_add_integ` · `fp_add_dig` · `fp2_is_zero(w)`: zero → `ep2_curve_get_gen` · `ep2_neg` iff the parity bit is set, then return. Else `bn_copy` (`ep2_curve_get_s3`) · `fp2_zero` ×2 · `fp_prime_conv` · `bn_copy` (`ep2_curve_get_s32`) · `fp_prime_conv` · `fp2_inv` · `fp2_mul_integ` ×2 · `fp2_neg` · `fp2_mul_integ` · `fp2_add_integ` · `fp2_neg` · `fp_sub_dig` · `fp2_mul_integ` · `fp2_inv` · `fp_add_dig` · `fp2_zero` · `fp2_set_dig` · (`fp2_copy` · `ep2_rhs` · `fp2_srt`) ×2 for x1 and x2 · then for the chosen index (0, 1 or 2) `fp2_copy` · `ep2_rhs` · `fp2_srt` · then `fp2_neg` · `fp_write_bin` ×2 · `ep2_neg(p, p)` iff the byte comparison differs from the parity | `legacy.cpp:150-304`; helpers `:17-51` |
| `MulCof(r, p)` = the static `ep2_mul_cof_b12` (inlined; no symbol) | `bn_make` · `fp_param_get_var` (static): `bn_make` · `bn_set_2b` · (`bn_set_2b` · `bn_add`) ×5 · `bn_neg`; then `ep2_mul_basic` ×2 · `ep2_sub` ×3 · `ep2_norm` · `ep2_frb` · `ep2_add_projc` · `ep2_dbl_projc` · `ep2_norm` · `ep2_frb` · `ep2_add_projc` · `ep2_norm` | `legacy.cpp:58-148` |
| `NV(n)` = `NativeVerify` | `fp12_zero` · `fp_set_dig` · `fp12_zero` · `fp_set_dig` · per chunk of 250: `pc_map_sim` · `fp12_mul` · then `gt_cmp`, and the code read on the same line. NE or a set code → clear, `false`; else [clr], `true` | `schemes.cpp:326-350` |

| Wrapper op | Steps, in Core's order | Source |
|---|---|---|
| `ADD` (`operator+`) | ctor · `g*_add(ans, a, b)` · [clr] | `elements.cpp:219-225`, `:457-463` |
| `NEG` | `Neg(a)` | as above |
| `MUL` (`operator*(G, bn)`) | ctor · `g*_mul(ans, a, k)` · [clr] | `elements.cpp:227-233`, `:465-471` |
| `AGG_PK`/`AGG_SIG`, pair (`AggregateInsecure(o)`, in place on the first register) | (the shim's copy of the first register is made before the window, D61) · ctor (`CoreMPL::Aggregate`'s `aggregated`) · for `impl`, then `o.impl`: `ep*_add_projc(acc, acc, x)` · [clr] | `bls.cpp:144-153`, `:222-231`; `schemes.cpp:176-192`; `elements.cpp:212-217`, `:450-455` |
| `AGG_PK`/`AGG_SIG`, span (`AggregateInsecure(span)`), empty | ctor (the wrapper `{}` that `return {}` builds), then return it: an invalid result | `bls.cpp:155-159`, `:233-237`; `bls/bls.h:50, 57` |
| `AGG_PK`/`AGG_SIG`, span, non-empty | ctor (the wrapper `ret`) · ctor (`aggregated`) · per input, in order: `ep*_add_projc(acc, acc, x)` · [clr] · then the **elision ctor** where the pinned build has one: the return slot's `CBLSWrapper` move constructor default-initializes `impl` before it swaps (`bls/bls.h:61-66`). In the pinned v24 build the G1 function has it (`ep_set_infty`) and the G2 function does not (§3.6) | `bls.cpp:161-176`, `:239-255`; `schemes.cpp:176-192` |
| `DH` (`DHKeyExchange`) | the wrapper's `IsValid()` checks are flags, not relic · `g1_copy(ans, pk)` · `g1_mul(ans, ans, k)`, aliased · ctor · `g1_copy` (`FromNative`), **with no [clr]** (D37); then `ToByteVector(false)`: `g1_write_bin` | `bls.cpp:208-220`; `privatekey.cpp:184-192`; `elements.cpp:94-99`, `:178-198` |
| `IMPL_EQ`; `EQ` | `g*_cmp(a, b)`. `EQ` compares the `fValid` flags first and makes that call only when they agree | `elements.cpp:200-203`, `:438-441`; `bls/bls.h:77-80` |
| `ISVALID` | the wrapper's flag, then `Valid(x)` | `elements.cpp:135-142`, `:358-365` |
| `SER_HASH` (`ToBytes(scheme)`, then `GetHash()`) | `ToBytes`: an invalid wrapper → zero bytes with no steps, else `ep*_write_bin`. Then `GetHash`: when the register's cache value is null (`cachedHash.IsNull()`), `SerializeHash` → `ToBytes(global flag)` → `ep*_write_bin` again for a valid wrapper (none for an invalid one), then the digest is stored; an all-zero digest leaves the value null, so the next `SER_HASH` serializes again. When the value is not null, no steps. So an uncached valid `SER_HASH` is two `ep*_write_bin` calls and a cached one is one (F10, D49). ABI 14's `SER` (op 1) is `ToBytes` alone | `bls/bls.h:125-146`, `:170-180`; `elements.cpp:178-198`, `:400-436` |
| `VERIFY`, basic (`CoreMPL::Verify`) | the wrapper's flags (no relic) · `Map(h)` · `Gen` · `Neg` · `Copy` · `Valid(pk)` → stop · `Valid(sig)` → stop · `Copy(pk)` · `Copy(sig)` · `Copy(hashed)` · `NV(2)` | `bls.cpp:296-307`; `schemes.cpp:135-153` |
| `VERIFY`, legacy (`LegacySchemeMPL::Verify`) | the wrapper's flags · `Gen` · `Neg` · `Copy` · `Copy(pk)` · `Copy(sig)` · `MapL(h)` · `Copy` · `NV(2)` | `schemes.cpp:658-669` |
| `VERIFY_AGG`, basic | the wrapper's flags: the signature, then each key (no relic) · the argument check (n ≥ 1 with equal counts: no relic) · distinct hashes (F9) → stop · then `CoreMPL::AggregateVerify`: the argument check again · `Gen` · `Neg` · `Copy` · `Valid(sig)` → stop · `Copy(sig)` · for each i: `Valid(pkᵢ)` → stop · `Copy(pkᵢ)` · `Map(hᵢ)` · `Copy` · then `NV(n + 1)` | `bls.cpp:314-339`; `schemes.cpp:416-431`, `:297-324` |
| `VERIFY_AGG`, legacy | the wrapper's flags · the argument check · `Gen` · `Neg` · `Copy` · `Copy(sig)` · for each i: `Copy(pkᵢ)` · `MapL(hᵢ)` · `Copy` · then `NV(n + 1)` | `schemes.cpp:683-703` |
| `VERIFY_SEC` (`VerifySecure(…, fLegacy = flag)`) | an empty span → `false` · `bn_make` per key (`BnArrayGuard`) · per key: `g1_write_bin` (`SerializeToArray(flag)`) · the sort (memcmp, no relic) · `HashPubKeys`: `bn_make` (`BnGuard`) · `ep_curve_get_ord` (`g2_get_ord`) · `md_map_sh256` (`Util::Hash256` of all keys) · per i: `md_map_sh256` · `bn_read_bin` · `bn_mod_basic` · then ctor (`publicKey`) · per sorted key: the re-decode `FromBytes` [ctor; `c0…` → no read; else `g1_read_bin` (decode, outside the D8 claim) · [clr], which throws iff basic; basic only: `Valid` → stop (throw) · [clr]] · ctor · `g1_mul(t, key, tᵢ)` · [clr] · `g1_add(acc, acc, t)` · [clr] · then the virtual `AggregateVerify({acc}, {h}, sig)`, as the `VERIFY_AGG` row for the flag (one hash, so F9 passes) | `bls.cpp:341-358`; `schemes.cpp:231-254`, `:256-259`, `:677-681`, `:32-58`; `bn_helpers.hpp:21-56`; `util.hpp:61-64`; `elements.cpp:24-92`, `:143-148`, `:212-233` |

`VerifyAggregateSignatureArguments`'s n = 0 branch (`signature == G2Element()`, a ctor and a `g2_cmp`) is unreachable from the wrappers: `VerifyInsecureAggregated` asserts a non-empty list, and `VerifySecure` always passes one key.

**Required path scripts (seeds, D59).** `AGG_PK` and `AGG_SIG` over an empty span, a one-input span and a multi-input span, plus the pair form; `SER_HASH` uncached, then again (cached), then after each cache-clearing op (F10). They make the log cover every constructor and cache path on the pinned build. They are §8 seeds; nothing compares their logs with a list.

`diff_bls_arith` and `diff_bls_verify` read the log and the code field on every input. They assert two things: every log entry outside the decode symbols is `RLC_OK`, and the post-op code is `RLC_OK`. A nonzero code stops the slice. The selftest and the throw-site script run in the slice's L0 suite and on every pin move. The engine models the code as never set.

### 3.4 Proving equality

| Check | Against | Scale | Slice |
|---|---|---|---|
| Engine vs blst on Usable inputs (all ops, random scalars incl. ≤ 64-bit, 0, r−1, r, > r) | blst, by Lemma U | `bls_engine_model` (model target, no oracle, about 10⁵ exec/s) | 3, 4 |
| Engine vs Core on Exotic and mixed inputs | the BLS script oracle (§7, ABI 15) | `diff_bls_arith`, `diff_bls_verify` | 3, 4 |
| P1–P5 and every class in §8's seed list as fixed vectors | oracle-recorded | L0 unit tests | 2–4 |
| Final exponentiation (Lemma E) | relic `pp_exp_k12`, scratch probe | 10⁶ | 4 |
| Group relic shape and Lemma T′ (§3.1) | Core's relic, `shape_probe.cpp` (done for the design, `NEG` of both infinity shapes included: 0 mismatches); then `bls_engine_model` (shape vs engine) and the `IMPL_EQ`/transition scripts in `diff_bls_arith` | probe 2.6·10⁴ comparisons; fuzzed | 3 |
| D8 (§3.3) | the real wrapper's call log and post-op code; the static throw-site table; the injection selftest | every `diff_bls_*` input; L0 and every pin move | 2–4 |

### 3.5 Source sweep (rev4, D38)

Since rev7 the trace rows below record the non-normative step lists (D59); the producer rows remain normative. Sol r3 found two kinds of mistake: an infinity special case missing from a producer row, and relic calls listed out of order. rev4 rechecked every producer row and every trace row against v24.0.0-rc.3 for both:

| Row | Checked against | Infinity special cases | Order | Result |
|---|---|---|---|---|
| decode | `elements.cpp:32-92` (the `c0…` early return), relic `ep*_read_bin` | `c0…` gives the default element (ctor, no read): `BASIC` (0, 0, 0) | — | matches |
| `Generator()` | `elements.cpp:127-133`; `ep*_curve_get_gen` | none | — | matches (`BASIC`, probe) |
| `g*_mul` | `relic_pc_exp.c:38-66`; `ep_mul_dig` (`relic_ep_mul.c:740-780`); `ep_mul_glv_imp` (`:42-135`); `ep2_mul_glv_imp` (`relic_ep2_mul.c:55-100`) | k = 0 or p infinite → `set_infty`; only a negative scalar of at most 64 bits ends with `g*_neg` (`relic_pc_exp.c:44-49`). A longer one goes through `bn_mod` to a non-negative value, then `ep*_mul` (`:52-60`; `relic_bn_div.c:57-118`), and off the curve that is not the negation of the magnitude's product (Sol r4 probe) | p is read into the table before `r` is written, so the aliased DH call is safe | matches. Negative scalars are outside the wrapper domain (every wrapper scalar comes from `bn_read_bin`, which is non-negative). The engine ports both negative paths as relic has them, but no certification claim covers them (D46) |
| `g*_mul_gen` | `relic_pc_exp.c:68-88`; `relic_ep_mul_fix.c:180` | k = 0 → `set_infty` | — | matches |
| `g*_add` with an infinity | `relic_ep_add.c:742-754`; `relic_ep2_add.c:390-407` | p is tested first: two infinities give an exact copy of q | — | **sharpened** (two rows) |
| `g*_add`, finite | R1a/R1b; T′ (c) | G1 (0, y ≠ 0, 0), G2 `set_infty` + `PROJC` | — | matches |
| `g*_neg` | `relic_ep_neg.c:38-52`; `relic_ep2_neg.c:39-52` | **infinite → `set_infty`, `BASIC`** | — | **fixed** (Sol r3 Major 1, D33) |
| `ToNative`/`FromNative` | `elements.cpp:94-99, 150-152, 319-324, 373-375`; `ep*_copy` | copies the tag | — | matches |
| map outputs | `ep2_map_dst`, `legacy.cpp:306-433` | — | — | not a register value: they reach only `pc_map_sim`, which normalizes |
| `SubInsecure` | `bls.cpp:289-294` | `Negate` then `+`: both rules apply | — | not a script op; covered by `NEG` and `ADD` |
| trace `ADD`/`NEG`/`MUL` | `elements.cpp:154-159, 212-233, 385-390, 450-471` | — | the ctor precedes the call | ctor steps added |
| trace `AGG` | `bls.cpp:144-176, 222-255`; `schemes.cpp:176-192` | — | unchanged | the empty-span case added |
| trace `DH` | `bls.cpp:208-220`; `privatekey.cpp:184-192` | — | unchanged | `FromNative` steps added; **no [clr]** (D37) |
| trace `EQ`/`ISVALID`/`SER` | `elements.cpp:135-142, 178-203, 358-365, 438-441` | — | unchanged | matches |
| trace `VERIFY`, basic | `schemes.cpp:135-153`; `elements.cpp:334-347` | — | **map, generator, key, signature** | **fixed** (Sol r3 Major 2) |
| trace `VERIFY`, legacy | `schemes.cpp:658-669` | — | **generator, key, signature copies, then the map** | **fixed**: rev3 listed the map before the generator for both flags |
| trace `VERIFY_AGG`, basic | `schemes.cpp:297-324, 416-431` | — | **signature once, then each key's check with its map** | **fixed** (Sol r3 Major 2); the distinct-hash stop added (F9, D36) |
| trace `VERIFY_AGG`, legacy | `schemes.cpp:683-703` | — | **each key's copy with its map** | **fixed**: rev3 listed the maps after all keys |
| trace `VERIFY_SEC` | `schemes.cpp:32-58, 231-259`; `elements.cpp:24-92, 143-148` | the `c0…` re-decode makes no read | unchanged | the re-decode and guard steps expanded; the inner call now runs F9 |
| trace `NV` | `schemes.cpp:326-350` | — | unchanged | the `fp12_zero`/`fp_set_dig` set-up and the code-read branch added |
| trace `AGG`, rev5 | `bls.cpp:144-176, 222-255`; `bls/bls.h:50-66`; the v23.1.8 bench build's `libbitcoin_consensus_a-bls.o` (GCC 12.2) | — | **wrapper ctors** | **fixed** (Sol r4 Major 1): split into pair and span, with `ret` and `{}` ctors. The build shows three `ep_set_infty` sites in `CBLSPublicKey::AggregateInsecure(Span)` (`{}`, `ret`, and the return slot's move after the aggregate) but two `ep2_set_infty` sites in the G2 one, so elision ctors are pinned per build (D43) |
| wrapper-ctor sweep, rev5 | every wrapper row: `bls.cpp:208-220, 296-358`; `bls/bls.h:77-80, 125-140` | — | — | no other `CBLSWrapper` is constructed inside a window. `DH`'s out-wrapper is built by the caller, the verifies return `bool`, and `std::vector<G*Element>` copies use the implicit copy constructor (no relic). The wrapper `operator==` (`fValid` first) was not a script op at rev5; since rev7.2 it is op 20 `EQ`, and dashbls `operator==` alone is op 26 `IMPL_EQ` (D73). `SER` of an invalid wrapper makes no call (added) |
| helper sweep, rev5 | `nm -A` on the pinned `libdashbls.a` objects; `legacy.cpp:17-433`; `util.hpp:61-64` | — | — | `ep2_sw_encode` (local `t`), `ep2_mul_cof_b12` (inlined), `fp_param_get_var` and the `ep2_curve_get_*` helpers are expanded (Sol r4 Major 2). `Util::Hash256` is inline `md_map_sh256`, so `HashPubKeys` gains its steps (D45). `ep2_map_dst`, NativeVerify's set-up and every other step symbol are `U` in their objects |
| F8 text, rev5 | `elements.cpp`, `privatekey.cpp:184-205` | — | — | **narrowed** (Sol r4 Minor 2, D47) |
| trace `SER`, rev6 | `bls/bls.h:125-146, 170-180` | — | **`GetHash`'s second write** | **fixed** (Sol r5 Major 1, D49) |
| implicit `GetHash`/`cachedHash` sweep, rev6 | `bls.cpp` (every `cachedHash` write), `bls/bls.h:85-88, 95-123, 141-146, 542-558` | — | — | no wrapper op in the ABI calls `GetHash` except `SER` (`SER_HASH` since rev7). The wrapper `operator<` hashes, but it is not an op. `CBLSLazyWrapper::GetHash` serializes only an unbuffered object, during register load, outside every window. Register load (`SetBytes`) also runs `impl == ImplType()` (a ctor and `ep*_cmp`) outside the window. The cache-clearing writes of ABI ops are listed in F10. The other `cachedHash` writes in `bls.cpp` (`:33, 40, 58, 79, 86, 108, 121, 139, 182, 204, 285, 363, 389`) are in functions that are not ABI ops: `CBLSId`, the secret-key functions, `GetPublicKey`, `Sign`, `PublicKeyShare`, `AggregateSecure` and `Recover`. The ADD/NEG/MUL results are new registers, so their cache starts null (D50) |
| build evidence, rev6 | §3.6 | — | — | **replaced**: rev5 read a v23.1.8 archive (Sol r5 Major 2). The pinned v24 build confirms the AGG constructor sites, `HashPubKeys` and the legacy-map call sites; `ep2_sw_encode` is now partly inlined (D51) |
| F1 text | `schemes.cpp:135-153, 297-324` | — | — | **fixed**: "no arithmetic happens first" was wrong (the map and the generator's negation run first); the Exotic-operand claim (D31) stands |

### 3.6 Build evidence (rev6, D51)

Every object-level fact in this design comes from the build that `dash-oracle` links: BS12's pin `v24.0`, Core commit `0dd12af0644b5b3aafc8d60cb2a786292d7788b0` (v24.0.0-rc.3 plus the shim), recipe `build-oracle.sh` at the `node/bs12-adapt` revision, GCC 15.2.0 and GNU ld 2.46 (the archive's MANIFEST). rev5's evidence came from a v23.1.8 archive and is withdrawn.

| Artifact | sha256 | How it was checked |
|---|---|---|
| `libdash_oracle_v24_0.a` (the pinned archive, `/work/cache/oracle/0dd12af0…/plain`) | `3af88a4b55009c08797ab79952f1c18d39264e94eeccde1607ca1ca9ac5ee18d` | equals `pins.txt` (BS12's branch) |
| its only member, `oracle_v24_0.o` | `8ecdb8d07442463ee010563582e4f8249a76a0a6a1f0d9fca8cd9af6aebc300e` | `objdump -dr` call relocations per function, below |
| a rebuild of the same pin with the build tree kept (scratch, recipe unchanged apart from not deleting the tree) | `3af88a4b55009c08797ab79952f1c18d39264e94eeccde1607ca1ca9ac5ee18d` | byte-identical to the pin, so the kept tree's archives are the pin's `ld -r` inputs (all `.a` under `src` and `depends/<host>/lib`). Its archives are deterministic (zero dates and ids) and its objects contain no build path |
| its `src/dashbls/.libs/libdashbls.a` (dashbls and relic members) | `e1aaccbbd4ffbd78c439b29eb2b816c8781559360a4f120807a3d1dcc4a512c9` | member hashes below |
| — `libdashbls_la-legacy.o` | `c5e227c3296fd72ccba4f63df02ef52b669f18d413eb7fc1115d887da889f7a3` | `nm`; `objdump -dr` |
| — `libdashbls_la-elements.o` | `7a7ca8345e947195b483954cd9715ecf7d93753cb26094ded4fc181bd7bfc948` | `nm` |
| — `libdashbls_la-schemes.o` | `d72a6b85584bb576af11975275923480490dd4779715033945cc93e5a5257308` | `nm` |
| — `libdashbls_la-privatekey.o` | `f4e561ee6dbb3ccd23c195e09ece39b7e9f1bde1889d576c22faf44a9013d29a` | `nm` |
| its `src/dashbls/.libs/librelic.a` | `be445a045037e82399667779700f07cfc384557dd91687c4016c01adef4b71be` | its 261 members are byte-identical to the relic members of `libdashbls.a`, so either archive resolves the same code |
| its `src/libbitcoin_consensus.a` | `087f777eca6d0b6163f9cfd8809d14a34a3988b00bbd2b0eb6b018c2923d2eb6` | holds Core's `bls` object |
| — `libbitcoin_consensus_a-bls.o` (`bls/bls.cpp`) | `37b6f5cfc4d1401f1232f908174729747af9cf5b8ae724d804466522a794cf99` | `nm`; `objdump -dr` |

Since rev7 these hashes are provenance only: no checker rebuilds the pin or compares object hashes (D59).

Facts read from these objects:
- **Wrap set (D60).** `nm`: the symbols that are `U` in the nine dashbls objects (`libdashbls_la-{bls,chaincode,elements,extendedprivatekey,extendedpublickey,legacy,privatekey,schemes,threshold}.o`) or in Core's `libbitcoin_consensus_a-bls.o`, and `T` in a `librelic.a` member: 84 symbols in the raw intersection. The wrap set excludes `core_get`, leaving 83, of which `core_init`, `core_set_thread_initializer`, `ep_param_set_any_pairf` and `err_get_code` run only at initialization; the other 79 are the throw table's roots (D70). The raw list, `core_get` included, is in `/work/scratch/bs11a/v24in/wrapset.all`. `CheckRelicErrors` is dashbls's own symbol (`U` in two dashbls objects), not relic's, so it is not in the set.
- **`U` symbols (rev6, D42; documentation since rev7).** Checked mechanically: every relic step symbol in the step lists, mapped to its linked name by the symbol rule's table, is `U` in the input object that compiles its cited file. `legacy.o`: `bn_add` `bn_copy` `bn_make` `bn_neg` `bn_read_bin` `bn_read_str` `bn_set_2b` `ep2_add_projc` `ep2_curve_get_gen` `ep2_dbl_projc` `ep2_frb` `ep2_mul_basic` `ep2_neg` `ep2_norm` `ep2_rhs` `ep2_set_infty` `ep2_sub` `fp2_add_integ` `fp2_copy` `fp2_inv` `fp2_is_zero` `fp2_mul_integ` `fp2_neg` `fp2_set_dig` `fp2_srt` `fp2_zero` `fp_add_dig` `fp_copy` `fp_prime_conv` `fp_sub_dig` `fp_write_bin` `md_map_sh256` (plus the excluded `core_get` and the non-relic `pthread_once`). `elements.o` has the `ep*`/`ep2*` read, write, compare, copy, add, negate, infinity and map symbols, `g1_mul`/`g2_mul`, `g*_is_valid` and `md_map_sh256`. `schemes.o` has `bn_make` `bn_mod_basic` `bn_read_bin` `ep_curve_get_ord` `ep*_set_infty` `md_map_sh256` and `NativeVerify`'s `fp12_zero` `fp_set_dig` `pp_map_sim_oatep_k12` `fp12_mul_lazyr` `fp12_cmp`. `privatekey.o` has `g1_mul`/`g2_mul`. Core's `bls` object has only `ep_set_infty` and `ep2_set_infty`. `ep2_sw_encode` is local (`t`, its `.part.0` clone), and `ep2_mul_cof_b12` and `fp_param_get_var` have no symbol (inlined), so the rule expands them (D41). `ep2_map_legacy` is dashbls's own C++ function (`T` in `legacy.o`, `U` in `elements.o`), so `MapL` expands its body
- **`legacy.o` call sites.** `ep2_map_legacy` holds the inlined `t = 0` tests of both `SwEnc` calls (`fp2_is_zero` ×2, `ep2_set_infty` ×2), `bn_make` ×4, `md_map_sh256` ×8, `bn_read_bin` ×4, `fp_prime_conv` ×4, `fp_copy` ×4, `ep2_add_projc`, and the inlined `MulCof`, whose static `fp_param_get_var` is inlined too (`bn_make` ×2, `bn_set_2b` ×6, `bn_add` ×5, `bn_neg`, `ep2_mul_basic` ×2, `ep2_sub` ×3, `ep2_norm` ×3, `ep2_frb` ×2, `ep2_add_projc` ×2, `ep2_dbl_projc`). The local `ep2_sw_encode…part.0` holds the rest of `SwEnc` in the listed order, with `pthread_once` for each `std::call_once`. The two once-lambdas (`ep2_curve_get_s3`, `_s32`) each call `bn_make` then `bn_read_str`.
- **Constructor sites (D43, D53).** `CBLSPublicKey::AggregateInsecure(Span)`: three `ep_set_infty` sites, one before the vector is built (`return {}`) and two after it (`ret`, and the return slot's move). `CBLSSignature::AggregateInsecure(Span)`: two `ep2_set_infty` sites (`return {}`, `ret`). The pair forms have none in Core's function; their only constructor is `CoreMPL::Aggregate`'s.
- **`HashPubKeys` (D45).** It is inlined into `CoreMPL::VerifySecure(…, bool)`: `bn_make` (guard) · `ep_curve_get_ord` · `md_map_sh256` (all keys) · per key `md_map_sh256` · `bn_read_bin` · `bn_mod_basic`, then the `publicKey` ctor and calls to `FromBytes`, `operator*` and `operator+=`.
- **`DH`.** `CBLSPublicKey::DHKeyExchange` calls the out-of-line `operator*(PrivateKey, G1Element)` (`privatekey.cpp`), as the step list says.

## 4. Secret keys (D12)

- **Canonical rules.** BS11a-t implements them already: legacy ⇔ 0 < v < r, and basic ⇔ 0 < v ≤ r with r stored unreduced and VALID (`privatekey.cpp:52-70`). `diff_bls_decode` covers sk under both schemes.
- **Arithmetic.** The secret's scalar is the stored integer. `g1_mul`/`g2_mul` reduce it mod r (R2), so an sk = r key gives infinity: `GetG1Element`, DH and signing all return the identity, which serializes as `c0 00…`. A scalar of at most 64 bits takes the `mul_dig` path, which the engine also ports.
- **Hygiene.** Scalars stay in `Zeroizing` buffers. Range checks stay constant time (BS11at-r1 §5).

## 5. IES key (D11)

**Core.** `symKey = ToByteVector(false)[0..32]` of `DHKeyExchange(sk, pk)` on encrypt and both decrypts (F4).
- It always uses the basic encoding, with 0x80 compression and 0x20 sign in byte 0, and `c0 00…` for infinity, whatever the flag, the scheme or the payload version.
- An invalid wrapper on either side makes `DHKeyExchange` return `false`, and `Encrypt`/`Decrypt` then fail.

**dash-pkc today.** The Chia arm writes the shared point in the legacy encoding (`ies_ops.rs` test `kdf_reads_the_scheme_own_encoding` asserts exactly that, and the `bls_dh` "chia" KATs encode it). That diverges from Core in byte 0.

**Fix.** There is one KDF: the basic serialization of the DH point, computed with the engine when the peer key is Exotic (S8, both directions). The `bls_dh` corpus is regenerated from the BLS script oracle (`DH` op, §7). The scheme tag on `BlsIesBlob` stays a pure wire-encoding tag for the ephemeral key. Multi-recipient composition stays in BS11b.

## 6. API in dash-pkc (D17, D20)

```rust
// dash_pkc::bls::core, in the `bls` feature. Every function is serial: no pooled blst entry point.
/// Core's wrapper state, `fValid` and `impl` held apart (F11, D69); BS11a-t's type, re-shaped in BS11a-1.
/// The element is a decoded one (basic bytes + `BlsPoint`), the default element, or from BS11a-3 an
/// arithmetic result. Equality is the wrapper's `operator==`: validity, then the element (D2; script op `EQ`, D73).
pub struct BlsValue<K, const N: usize> { /* valid: bool, element */ }
impl<K, const N: usize> BlsValue<K, N> {
    pub fn invalid() -> Self;                                  // fValid = false, impl = the default element
    pub fn invalidate(&mut self);                              // fValid = false, impl kept (a failed in-place op)
    pub fn is_valid(&self) -> bool;                            // fValid
    pub fn to_bytes(&self, scheme: BlsSchemeKind) -> [u8; N]; // zeros when invalid, whatever the element
}
pub type BlsPkValue = BlsValue<G1, 48>;   // existing BS11a-t types
pub type BlsSigValue = BlsValue<G2, 96>;
pub type BlsSkValue = BlsValue<Fr, 32>;

/// CBLSSignature::VerifyInsecure(pk, h, flag): F1 under Basic, relic's pairing under Legacy.
pub fn verify_insecure(sig: &BlsSigValue, pk: &BlsPkValue, h: &[u8; 32], flag: BlsSchemeKind) -> bool;
/// CBLSSignature::VerifyInsecureAggregated; Core asserts equal non-empty lengths, here that is `false`.
/// Basic flag: equal hashes → `false` before any arithmetic (F9); legacy: no such check.
pub fn verify_insecure_aggregated(sig: &BlsSigValue, pks: &[&BlsPkValue], hs: &[[u8; 32]], flag: BlsSchemeKind) -> bool;
/// CBLSSignature::VerifySecureAggregated: HashPubKeys weights, F2/F3. Like Core, no validity gate:
/// an invalid signature or key enters with the element it holds (F11).
pub fn verify_secure_aggregated(sig: &BlsSigValue, pks: &[&BlsPkValue], h: &[u8; 32], flag: BlsSchemeKind) -> bool;
/// The IES symmetric key Core derives: None when DHKeyExchange fails (an invalid object).
pub fn ies_key(sk: &BlsSkValue, pk: &BlsPkValue) -> Option<Zeroizing<[u8; 32]>>;

/// A G1/G2 element as Core's dashbls holds it, the result of arithmetic (for BS11b).
pub struct BlsElem<K> { /* Group(blst value + Core's relic coord tag, §3.1) | Relic(engine point) */ }
impl<K> BlsElem<K> {
    pub fn from_value(v: &BlsValue<K, N>) -> Option<Self>; // Lemma C; None for an invalid value
    pub fn from_impl(v: &BlsValue<K, N>) -> Self;          // the wrapper's impl, valid or not (F11)
    /// CBLS*::AggregateInsecure(Span): None for an empty span (Core's invalid `{}`); else the left
    /// fold from the default element over `from_impl` of each input, in slice order (F11, D63, D69).
    pub fn aggregate_insecure(vs: &[&BlsValue<K, N>]) -> Option<Self>;
    pub fn add(&self, o: &Self) -> Self;                   // g*_add; off the curve, order matters (D63)
    pub fn neg(&self) -> Self;
    pub fn mul(&self, k: &[u8; 32]) -> Self;               // g*_mul on a 256-bit big-endian scalar
    pub fn is_valid(&self) -> bool;                        // dashbls IsValid: infinity → true
    pub fn eq(&self, o: &Self) -> bool;                    // dashbls operator== (g*_cmp, §3.1 audit; D30); script op IMPL_EQ
    pub fn to_bytes(&self, scheme: BlsSchemeKind) -> [u8; N]; // SerializeToArray
}
```

- **Flags.** `flag` is the verify flag: the caller's `BlsCtx` (A1 §1.2), never the decode scheme. The decode scheme stays on the `BlsValue`'s origin (EV1 D22).
- **The all-Usable path still follows `NativeVerify`'s structure (D9).** Pairs with an infinity side are skipped, `IsValid(∞)` is true, and an empty key list follows `VerifyAggregateSignatureArguments`. The basic flag's distinct-hash rule (F9) applies on every path. So an aggregate that sums to infinity gives Core's answer, not blst's `PK_IS_INFINITY`.
- **Invalid values (F11, D62, D69).** `verify_insecure` and `verify_insecure_aggregated` return `false` on an invalid input, as Core does. `verify_secure_aggregated` and `aggregate_insecure` use its element with no gate: the default element for a fresh, reset or failed-decode value, and the kept element after a failed in-place op. `aggregate_insecure` returns `Some` for any non-empty span: Core's `fValid = false` there needs a relic throw, which D8 excludes. `from_value` is for callers that hold only valid values. The pair `AggregateInsecure(o)` is `aggregate_insecure(&[a, b])` on two valid values (D63); its throw branch, which keeps the pre-call element, also needs a relic throw.
- **In-place ops (D69).** Ports of `SecretKeyShare`, `PublicKeyShare`, `Recover` and `DHKeyExchange` on a receiver (BS11b, LQ) call `invalidate` on every `false` return, so the receiver keeps its element as Core's does, and assign the element only on success. BS11a's own `ies_key` uses a fresh receiver, as Core's IES code does.
- **Fold order (D63).** Off the curve, relic's `g*_add` is commutative only up to a T′ rescaling and is not associative, so an aggregate's value depends on the fold.
  - `aggregate_insecure` folds left from the default element in slice order, as `CoreMPL::Aggregate` does (`schemes.cpp:176-192`).
  - A pair call is (∞ + a) + b. Adding to an infinite p copies q exactly (§3.1), so a chain of pair calls equals the span fold over the same order.
  - Callers reproduce Core's order (A-10):
    - The batch verifier aggregates keys per message hash. Hashes go in the order of `byMessageHash`, a `std::map<uint256, …>` built in `MessageId` order; within a hash, messages go in `MessageId` order, with duplicates skipped. The signatures are aggregated in the same traversal (`bls_batchverifier.h:24, 32, 73-76, 135-178`). Its secure step takes the last message per hash, in hash order (`:195-228`).
    - `bls_worker.cpp:840-860` uses job order, starting from the first job's signature and skipping cancelled jobs.
    - `net_dkg.cpp:255-283` uses message order.
  - **Excluded:** `CBLSWorker`'s parallel `Aggregator`/`VectorAggregator` (`bls_worker.cpp:120-400`), used only by its DKG computations (`:488-489`, `:602`, `:631`). It combines batch results in completion order. On inputs outside the group its value depends on thread scheduling, so it is not ported; on group inputs the order does not change the value.
- **Pure-crate safety (DEC-71/79).** The functions use only `blst_p1*`/`blst_p2*` add/double/mult on subgroup points, `blst_miller_loop`, `blst_fp12_mul`, `blst_final_exp`, `blst_fp12_is_one`, the C multi-scalar multiplication `blst_p1s_mult_pippenger` with a caller-provided scratch buffer (D64), and field FFI, all serial.
  - A selftest asserts that `/proc/self/task` does not grow across each function (DEC-79 pattern).
  - `contrib/node/pure-clippy.toml`'s audit stamp is re-audited for the new pin.
- **One serial verify (D22).** The pairing-product check over Usable operands (blst, `NativeVerify`'s structure per D9) is BS12's DEC-79 serial pool-free verify. BS11a does not write a second one.
  - BS11a-4 adds a branch inside that function: if any operand is Exotic or a non-group engine result, the engine computes the Miller values and the verdict (Lemma E). Only the legacy flag reaches it with such an operand: under basic, the F1/F2 gates admit only subgroup points. An aggregate that passes the basic gate (F7) is a subgroup point and is verified as one, after normalization, by Lemma U.
  - The three Core-shaped wrappers above (the flag gates F1–F3, `HashPubKeys`, the re-decode of F2/F3) sit on top. If BS12 ships some of them first, BS12's names and signatures win, and BS11a-4 adds only the legacy-flag and Exotic behaviour.

## 7. Oracle (D13)

**What exists.**
- `oracle_bls_decode` covers decode completely: pk, sig and sk; eager and lazy; both schemes; outcome plus re-serialization. BS11a-1 needs nothing new.
- `oracle_bls_verify` covers only `VerifyInsecure` with the verify flag equal to the decode scheme.
- **`oracle_bls_exec` (ABI 14) is already at the pin** (`oracle.h:74`, `:596-660`). It is the program's single BLS script entry (N13, DEC-70).
  - Ops 0–19, including DECODE 0, SER 1 (`ToByteVector` only), VERIFY 4, VERIFY_AGG 5, VERIFY_SEC 6, AGG_PK 8, AGG_SIG 9 (span) and DH 15.
  - A flags byte per op: `ORACLE_BLS_FLAG_LEGACY 0x01`; the other bits are reserved and must be clear.
  - A record per op: `int32 status`, `int32 reserved` (0; "relic's error code at a later ABI"), then a CompactSize-prefixed output.
  - Op codes 20–255 are free.
  - Registers hold whatever Core decoded, invalid objects included; DECODE leaves a register empty only when Core's decoding throws.

**BS11a-2 extends it at ABI 15 (D61).** It adds no entry point. Ops that exist are reused, and op 1 does not change:

| Addition | Encoding | Core call or semantics |
|---|---|---|
| Per-op flag | the existing `ORACLE_BLS_FLAG_LEGACY` bit | the shim sets `bls_legacy_scheme` for the op and restores it afterwards (as ABI 14 does) |
| relic's code after the op | the record's `reserved` int32 | `core_get()->code` after the wrapper returns: the post-op read (§3.3) |
| Call log | flags bit `0x02` (`LOG`). After the output: a CompactSize count, then per entry the symbol's name (CompactSize-prefixed) and an int32 code. Without the bit, the record is byte-identical to ABI 14 | §3.3 (D59, D60) |
| Pair `AGG` | flags bit `0x04` (`PAIR`) on ops 8 and 9: exactly two registers. d gets a copy of r[0], made outside the window, then `d.AggregateInsecure(r[1])`. An invalid operand is `ORACLE_E_ARG`, because Core asserts | `AggregateInsecure(o)` (D40) |
| In-place receiver | flags bit `0x08` (`INPLACE`) on ops 12, 13, 14 and 15: a register `recv` precedes the op's operands. The receiver is a copy of `recv`, made outside the window, instead of a fresh wrapper; Core's call runs on it and d gets it. A `recv` of the wrong kind is `ORACLE_E_ARG`. Without the bit the op is ABI 14's | `SecretKeyShare`, `PublicKeyShare`, `Recover`, `DHKeyExchange` on a used wrapper (F11, D69) |
| Span `AGG` with invalid registers | ops 7, 8, 9 and 11 as they are | `AggregateInsecure(span)`, `AggregateSecure` (F11) |
| `VERIFY`, `VERIFY_AGG`, `VERIFY_SEC` on Exotic and invalid registers | ops 4, 5 and 6 as they are | `VerifyInsecure`, `VerifyInsecureAggregated`, `VerifySecureAggregated` (F1–F3, F11) |
| `DH sk pk → d` | op 15 with the legacy bit clear: the output is `ToByteVector(false)`, and the IES key is its first 32 bytes | `DHKeyExchange` + `ToByteVector(false)[..32]` (F4) |
| `EQ r1 r2` | new op 20 on two registers of the same kind (else `ORACLE_E_ARG`); the output is one byte, 1 when equal | the wrapper `operator==`: `fValid == r.fValid && impl == r.impl`, so dashbls `operator==` runs only when the flags agree (`bls/bls.h:77-80`, D73) |
| `IMPL_EQ r1 r2` | new op 26, operands and output as op 20 | dashbls `operator==` (`g1_cmp`/`g2_cmp`) on the two `impl`s, whatever the flags; the comparison `BlsElem::eq` ports (D30, D73) |
| `ADD r1 r2 → d`, `NEG r → d`, `MUL r k → d` | new ops 21, 22 and 23; k is a 32-byte big-endian scalar, read with `bn_read_bin` outside the window | dashbls `operator+`, `Negate`, `operator*(G, bn_t)` (relic `g*_add`, `g*_neg`, `g*_mul`) on the operands' `impl`s. An invalid operand contributes its `impl` (F11). d is a wrapper whose `impl` is the result, with `fValid = true` (as `AggregateInsecure` sets it) and its cache null; its output follows ABI 14's destination ops |
| `ISVALID r` | new op 24; the output is two bytes, the wrapper's `fValid` and then dashbls `impl.IsValid()` | wrapper and dashbls `IsValid` |
| `SER_HASH r` | new op 25; the output is `ToByteVector(scheme)`, then the 32-byte `GetHash()` | `ToBytes(scheme)`, then `GetHash()`, in that order. The script model gives every register Core's cache value (32 bytes; all zero is null): it is null on load and after every op that writes the register (F10), and `SER_HASH` stores the digest it computed. "Cached" means the value is not null, read after the store. A separate flag, if the model keeps one, is derived from that and is never set by `SER_HASH` directly (D49, D58) |

**Required composed scripts** (scripts already compose ops; these are seed scripts, not new ops). Element comparisons (the transition, §3.1 boundary and fixed-vector scripts, here and in §8) use `IMPL_EQ`; register comparisons that may involve an invalid wrapper use `EQ`; the invalid-register and kept-element scripts use both (D73):
- **Aggregate, then verify (D26):** `AGG_PK`/`AGG_SIG` over Exotic and mixed registers, then `VERIFY`/`VERIFY_AGG` on the **result** with the basic flag (and with the legacy flag), mirroring F7's order. Testing `AGG` and `VERIFY` separately does not cover this path.
- **Duplicate hashes (D36):** `VERIFY_AGG` with two equal hashes under both flags, on Usable and Exotic keys.
- **Group → relic transition (D27, D30, D33):** Usable `ADD`/`MUL`/`NEG`, then Exotic `ADD`/`MUL` in both operand orders, then `SER_HASH`/`ISVALID`/`VERIFY`/**`IMPL_EQ`**. `NEG` is applied to both infinity shapes before the Exotic op and before `IMPL_EQ`, with Sol r3's vector `IMPL_EQ(NEG(G + (−G)), MUL((0, 2), k))` = EQ as a fixed step. Also **`IMPL_EQ` directly between Usable results and Relic values**, in both orders. These run for G1 and G2, and cover every row of §3.1's shape table: the `BASIC` and `PROJC` infinities, `G + (−G)`, and a zero-z engine result (P2's artifact, a doubled off-curve G2 point).
- **Invalid registers (D62):** `VERIFY_SEC` with an invalid signature, invalid keys (first, middle, last, all) and both, among Usable and Exotic keys; `AGG_PK`/`AGG_SIG` spans with invalid registers first, in the middle and as every input, then `VERIFY`/`VERIFY_AGG`/`SER_HASH` of the aggregate; `ADD`/`MUL`/`EQ`/`IMPL_EQ` on invalid registers. All run under both flags.
- **Kept-element registers (D69),** under both flags. Producers: `INPLACE` `SK_SHARE`/`PK_SHARE` with an invalid `msk`/`mpk` member and with fewer than two; `INPLACE` `RECOVER_SIG` with empty or unequal lists, an invalid share, fewer than two shares, a zero id and duplicate ids; `INPLACE` `DH` with an invalid key on either side. Receivers: basic-decoded and legacy-decoded registers (Usable, on the curve outside the subgroup, off the curve), engine results, the default element, and a register already invalid with a kept element (a chain). Each result then feeds `ISVALID`; `SER_HASH`; `EQ` and `IMPL_EQ` against its receiver and against a fresh invalid register (wrapper and element equality differ here); span `AGG_SK`/`AGG_PK`/`AGG_SIG` as the first, a middle and every input; `AGG_SECURE`; `VERIFY_SEC` as the signature and as keys; `ADD`/`NEG`/`MUL`; and the gated ops (`VERIFY`, `VERIFY_AGG`, `PUBKEY`, `SIGN`, `SUB_SIG`, `DH`). The pair `AGG` throw branch has no seed: it needs a relic throw (D8).

ABI 14's format already has the room: free op codes, reserved flag bits and the reserved int32 (A-7 closed, D61).

**ABI and building.**
- ABI 15, on a Core shim branch from the pin `0dd12af0644b` (ABI 14), v24.0 only (DEC-82).
- The `--wrap` seams sit in the plain flavor. If the log costs more than 5% (`perf stat -e instructions:u` over the existing op set), it moves to a trace flavor built by the same recipe (D56).
- Both flavours are installed in `/work/cache/oracle` before cleanup. A pin move without install broke every gate after O2a-LQ1 merged.

## 8. Verification

| Target | Kind | Compares | Seeds | Cert |
|---|---|---|---|---|
| `diff_bls_decode` | l1, v24.0, private | outcome + re-serialization under both schemes; lazy op scripts (BS11a-t's corpus scripts) | `bls_wrapper.json5` + a class-aware mutator (pick an A1 §1.4.1 class, then mutate coordinates, flags and stray bits) | DEC-186 |
| `bls_engine_model` | model | engine vs blst on Usable inputs, every op, scalar classes; the Group shape (§3.1, the `NEG`-of-infinity reset included) vs the engine's own coordinates for the same op sequence, up to T′'s rescaling | random | DEC-186 |
| `diff_bls_arith` | l1, v24.0, private | `ADD`/`NEG`/`MUL`/`AGG`/`DH`/`SER`/`SER_HASH`/`ISVALID`/`EQ`/`IMPL_EQ` scripts over mixed registers, including the transition and `IMPL_EQ` scripts (D27, D30, D73); bytes in both schemes, IES key, `EQ`, `IMPL_EQ`; invalid and kept-element registers (F11, D69); call-log codes `RLC_OK` outside decode and the post-op code `RLC_OK` (D59) | Exotic classes below + Usable | DEC-186, `min_certify_execs` 2·10⁵ |
| `diff_bls_verify` | l1, v24.0, private | the three verifies × flag × decode scheme, aggregate-then-verify scripts (D26) and duplicate-hash scripts (D36), invalid-register and kept-element scripts (D62, D69); call logs and post-op codes as above | same classes + signatures made from fuzzer-chosen keys, so Usable branches also say `true` | DEC-186, `min_certify_execs` 2·10⁵ |

**Exotic seed classes** (generator `gen.py`, extended):
- every ℓ-power torsion part of h₁ and h₂ (BS11at-r1), alone and plus a subgroup point;
- the G1 order-3 points (0, ±2) (P2);
- off-curve G1 x with s ∈ {0, 1} and off-curve G2 x (y = 0);
- pairs whose torsion parts cancel under addition, also as aggregate-then-verify inputs under the basic flag (D26);
- engine results fed back as operands;
- Usable results fed into Exotic operations and `IMPL_EQ` (the §3.1 boundary): every row of the shape table, and a zero-z engine result, in G1 and G2;
- Sol r2's fixed vector: `IMPL_EQ(G + (−G), MUL((0, 2), k))` with k = `3a5a…5a` (expected NE), and its BASIC-infinity counterpart `IMPL_EQ(MUL(G, 0), MUL((0, 2), k))` (expected EQ);
- Sol r3's vector: `IMPL_EQ(NEG(G + (−G)), MUL((0, 2), k))` with the same k (expected EQ: `NEG` resets the `PROJC` infinity to `BASIC`), and the G2 counterpart, `NEG` of a `g2_add` infinity against a zero-z engine result;
- `VERIFY_AGG` with equal hashes, under both flags (F9);
- invalid registers (F11, D62, D69): §7's invalid-register and kept-element scripts, under both flags;
- §3.3's required path scripts (span sizes, the pair form, the `SER_HASH` cache sequence);
- scalars 0, 1, 2⁶⁴−1, 2⁶⁴, r−1, r, r+1, 2²⁵⁶−1, and scalars near `bn_rec_glv`'s rounding boundaries.

The generator keeps its two-probe agreement (v24.0 and v23.1.8, identical sources) as a cross-check. Gates use v24.0 only (DEC-82).

**Farm.** Gates are "plateau with 0 divergences, 0 crashes" (DEC-184), certified by `fuzzctl certify` (DEC-186). Each target's PR smoke is 60 s; nightly weights are 2 (decode) and 1 (the rest). PLAN's "≥ 72 CPU-h" becomes the reported CPU figure only.
- A low-ceiling plateau triggers the coverage review DEC-184 (3) asks for, against an **engine branch list**: every relic branch the engine ports (infinity and zero-z paths, mixed vs general add, `mul_dig`, chunk boundary, pair skipping).
- An unhit branch gets targeted seeds, not more hours.

**L0.** P1–P5 as oracle-recorded vectors; dashbls `test.cpp` vectors through BS12 when they land; the regenerated IES KATs.

**Quirks.**
- Quirk 35 (BS11a-t, also BS11a) gains the G2 sign ∧ y.c1 = 0 row.
- **Quirk 45** (new; owner BS11a, status `scaffold` until BS11a-4) covers this: the three direct verify entry points return `false` on an Exotic operand under the basic flag with no arithmetic on that operand, and compute through relic's arithmetic under the legacy flag; aggregation and the IES DH key compute through relic under either flag, so aggregate-then-verify paths apply the basic gate to the aggregate only. The registry row is in this branch's docs commit (D23).

**Bench gate (B1 microbench, `perf stat -e instructions:u`, not wall clock):**
- `BlsEager::decode` legacy pk/sig must not be slower than today's `deser_pk`/`deser_sig::<BlsScChia>`. Basic must not be slower than `::<BlsScIetf>`.
- **The cost of the relic shape (D27):** `BlsElem::add`/`neg`/`mul` on Usable inputs, against the bare blst calls. The shape is one tag byte, updated by a branch per op; there is no relic run and no replay. Gate: at most 2% more instructions.
- **Legacy-flag verify against dashd (D64), gated.** These are PF0 `instructions:u` rows. The bar is the CHARTER's: never slower than dashd, with a target of 2×.

  | Bench | Input | Counter |
  |---|---|---|
  | `bls_verify_insecure_legacy` | `verify_insecure`, 1 key, all Usable, legacy flag; keys pre-resolved and unresolved | ≤ dashd |
  | `bls_verify_secure_legacy_50` | `verify_secure_aggregated`, 50 keys, all Usable, legacy flag; both key states | ≤ dashd |
  | `bls_verify_secure_legacy_400` | as above, 400 keys | ≤ dashd |

  - **Measuring Core.** A counter harness links a build of the pin made by the same recipe without the `--wrap` seams (so the seams' cost cannot loosen the bar). It runs `oracle_bls_exec` on a script that loads the registers and then runs one `VERIFY`/`VERIFY_SEC` op, minus the same script without that op.
  - **Route to the bar.** The pairing runs on blst. All-Usable weighted sums use blst's multi-scalar multiplication, which Lemma U makes exact. Classification is memoized where keys live: `BlsValue::resolve(&mut self)` turns `OnCurve` into Usable or Exotic once, and the MN-list and quorum key caches hold resolved values (A-11). Core runs no subgroup test under the legacy flag, which is why the unresolved rows exist.
- Reported only: `BlsElem::eq` Group-vs-Relic (one transliterated `ep*_cmp`). Engine paths are not benched.

**History scan (h)** (A1 §4, BSX Q-6) is evidence only and optional (D21). research/O6-scan-h.md counted the current mainnet list (0 Exotic) but not history, and its classifier predates BS11a-t. Slice 4 carries a non-blocking item: rerun (h) over history in blk mode with the BS11a-t classifier (on-curve and off-curve Exotic separately), as aggregate counts only.

## 9. Slices

| Slice | Content | Checkpoint | Unblocks |
|---|---|---|---|
| **BS11a-1** decode closure (M) | D2/D3 in `wrap_codec.rs`; `BlsValue`'s validity/element split (D69); the A1 §1.4.1 row; generator classes; `diff_bls_decode` on the existing `oracle_bls_decode`; bench row; quirk 35 rows | `diff_bls_decode` certified, bench pass, Sol → Opus | **EV1-2** (D10: exact legacy decode). EV1 verifies only from EV1-4, and the legacy-flag verify parity needs BS11a-4 |
| **BS11a-2** oracle ops (M) | **Starts now: ABI 14 is at the pin (D61).** The §7 ops, flag bits and code field at ABI 15 + `dash-oracle-sys` binding; the `nm`-derived wrap set, the generated `__wrap_` seams in the recipe's `ld -r` step, the call log and its injection selftest (D59, D60); the throw-site script (D65); reproducible archives; install; P1–P5 vectors | `check-oracle-repro.sh` OK; pins installed; vectors committed | 3 (the oracle half), 4 |
| **BS11a-3** engine groups + IES (L) | field tower, recoding, G1/G2 ops and `mul`, `SerializeToArray`, `BlsElem` (with `from_impl` and `aggregate_insecure`, F11, D63) and `BlsValue`'s arithmetic-result element (D69); `ies_key` + Chia KDF fix + KAT regen; `bls_engine_model`, `diff_bls_arith` | both certified, 0 error codes | BS11b, LQ4a (IES) |
| **BS11a-4** pairing + verify (M/L) | Miller loop port, Lemma E probe, the engine branch inside BS12's serial verify, the Core-shaped wrappers BS12 does not ship (D22), no-thread selftest, pure-clippy stamp; `BlsValue::resolve` and the D64 rows against dashd; `diff_bls_verify`; quirk 45 to `tested`. **Non-blocking:** scan (h) rerun (§8) | certified; selftest green; D64 rows ≤ dashd (scan (h) does not gate) | CH7b acceptance, EV1-4 legacy-flag parity (EV1's A-4), LQ3 |

Order: 1 first. 3 starts on `bls_engine_model` at once (no oracle needed); 2 starts now (D61), and 3's `diff_bls_arith` waits for 2. Then 4, which also needs BS12's serial verify. **Slice 1 depends on nothing** (no new oracle, no engine, no BS12), so EV1-2 is never held by the Exotic work.

## 10. Decisions

| D | Decision | Rejected | Why |
|---|---|---|---|
| D1 | No relic decode port; predicate + blst (BS11a-t) is the decode | PLAN's `ep_read_bin`/`fp_srt` port | BS11at-r1 §2 proof + 305k-input differential; Lemma C keeps relic's point |
| D2 | Lazy legacy classification (`Usable`/`OnCurve`/`OffCurve`); classification is not part of equality | eager `in_g*` at decode | bench gate; Core decides validity without subgroup |
| D3 | Off-curve coordinates rebuilt from canonical bytes on demand | storing coordinates in `BlsPoint` | keeps `Copy`/`Eq`; Lemma C |
| D4 | **Revised by D66 (Opus r1 m5):** one dispatch rule by representation; this row's wording is kept for history. Any Exotic or non-group operand → engine; all Usable → blst; Group values carry Core's relic shape across the boundary (D27) | maths shortcuts for on-curve Exotic | R2/R3 (P2): even on-curve points need relic's recoding and zero-z handling |
| D5 | Transliterate the listed relic functions verbatim, as configured | blst for everything; a general "relic-like" re-implementation | off-curve results depend on the exact op sequence (R1, R4); PLAN §7 ruling: port Core verbatim |
| D6 | Exact tower over blst's serial field FFI, inv(0) = 0 at every level | a pure-Rust field | fast, same Montgomery R as relic; zero case explicit (P1) |
| D7 | Final exponentiation not ported; verdict per Lemma E, FE(0) = 0 | port `pp_exp_k12` | invariance of "= 1" under fixed k; probe obligation in slice 4 |
| D8 | **Observation revised by D59 (Opus r1 M1):** the real wrapper's call log, not the raw traces; the claim is unchanged. For normal, successfully allocated inputs, no relic call outside decode sets the error code on the ported paths; decode codes are part of the decode outcome (BS11a-t); the `ERR_NO_MEMORY` branches are out of scope. Observed by the raw traces (D28) at depth-0 steps, where relic's sticky code shows any deeper set (D34); a nonzero code is a finding | modelling thread-local error state; "never set" without the allocation and decode qualifiers | a set code would make Core history-dependent, which cannot be ported (Sol r1 Major 2; r2: decode scope) |
| D9 | The all-Usable path keeps `NativeVerify`'s structure (skip ∞ pairs, `IsValid(∞)`, empty-list rules) | blst's verify API | blst's infinity and empty rules differ from Core's |
| D10 | BS11a owns the legacy-flag and Exotic semantics of the three verifies (`HashPubKeys` and F2/F3 re-decode included) and `ies_key`; the Usable pairing check is BS12's (D22); BS11b composes aggregates, threshold and multi-recipient IES on `BlsElem` | secure weights in BS11b | S2 is consensus (CH7b) and needs the engine (rev1) |
| D11 | One IES KDF: basic encoding of the DH point, first 32 bytes | per-scheme KDF (dash-pkc today) | `bls_ies.cpp:47, 83, 100` |
| D12 | Scalars reduced mod r before `mul`; ≤ 64-bit scalars through the ported `mul_dig` path | always LWNAF | `relic_pc_exp.c:38-66`; sk = r gives infinity as in Core |
| D13 | **Mapped onto the pin's ABI 14 by D61 (Opus r1 m1).** No BS11a entry point: BS11a-2 extends BS12's multi-op BLS script entry (ABI 14) at ABI 15 with per-op flag, per-op relic error code, and arithmetic/validity ops on Exotic registers (§7) | a separate `oracle_bls_exec` (r0); one entry per operation | manager ruling (rev1): one BLS script entry for the program |
| D14 | Four targets (three l1 v24.0 private, one model); DEC-184/186 certification | CPU-hour budget | DEC-184/186; DEC-82 |
| D15 | Slice 1 = decode closure, independent of the oracle and the engine | one slice | EV1-2 needs only exact decode |
| D16 | "Exotic → `false`" holds only for the three direct verify entry points under the basic verify flag; A1 §1.4.1 / D58 / §4.4, EV1 D2 / §2.5 and quirk 45 are reworded to say so | keeping A1's statement; an unscoped basic-flag claim | F1–F3, F7 (rev2: Sol r1 Major 1) |
| D17 | Serial entry points only; no-thread selftest; pure-clippy stamp re-audit | `no-threads` feature | DEC-71/79 |
| D18 | Rust wrappers are always fresh (Core's stale-`obj` reuse stays unmodelled) | modelling it | BS11at-r1 N1; no decode site reuses |
| D19 | v23.1.8 is a corpus cross-check only | v23.1 legs | DEC-82; identical sources |
| D20 | `dash_pkc::bls::core` (public functions on `BlsValue`, `BlsElem`) + private `dash_pkc::bls::relic` engine, both under the `bls` feature | new feature flag | no new blst surface; the engine is internal |
| D21 | History scan (h) is not a dependency; it is an optional, non-blocking item of slice 4 (§8) | parity resting on "no historical Exotic"; a blocking scan | the engine gives parity; the scan remains evidence (manager ruling, rev1) |
| D22 | One serial verify: BS12's DEC-79 pool-free verify is the Usable path; BS11a-4 adds the engine branch for Exotic operands inside the same function; where BS12 ships a Core-shaped wrapper first, its API wins | a separate BS11a pairing check | manager ruling (rev1); one implementation of the blst path |
| D23 | New quirk 45 (Exotic arithmetic under the legacy verify flag), owner BS11a, `scaffold` until BS11a-4; the register row and its ignored scaffold test (`conformance/tests/quirks.rs::quirk_45_exotic_bls_operands_by_flag`) are added in this branch | reusing quirk 35 | 35 is decode outcomes; 45 was the next free ID on every node branch at rev1 |
| D24 | The PLAN/A1/EV1/COVERAGE corrections ride on this branch as a separate docs commit, worded generically (no scenarios) | asks for the manager to apply | manager ruling (rev1): the merge carries them |
| D25 | **Superseded by D61 (Opus r1 m1):** ABI 14 is at the pin. BS11a-2 waits for BS12's ABI-14 commit; BS11a-3's model target does not | BS11a-2 first | §7: the additions slot into BS12's format |
| D26 | Aggregate-then-verify is its own tested path: §7 composed scripts and §8 seeds run `AGG_*` on Exotic inputs and verify the aggregate under the basic (and legacy) flag | testing `AGG` and `VERIFY` only separately | F7: the basic gate sees only the aggregate (Sol r1 Major 1) |
| D27 | Every `Elem::Group` carries Core's relic coord tag (and so its infinity shape) per §3.1's table, coordinates fixed up to the rescaling Lemma T′ proves unobservable (G2 mixed/general at λ = 2 included). The engine rebuilds Group operands from it; the model target checks the tag against the engine's own run | r2's affine `BASIC` rebuild (Sol r2's G1 EQ counterexample); lazy replay of the relic transliteration from retained operands (keeps an op DAG with secret scalars; unbounded cost when forced); an eager shadow engine on every Usable op (5–20× the hot path) | one byte per value, exact by T′ and the shape probe (manager ruling: coordinate preservation is the design) |
| D28 | **Superseded by D59 (Opus r1 M1).** D8 is observed by per-wrapper raw traces from pre-call snapshots (same flag, order, accumulator start and aliasing; the secure re-decode/weight/mul/add stage, the aggregate loop, DH's aliased mul), reading the code after every relic call, with a trace-fidelity check (D35: final value, the real wrapper's call log, derivation from the cited lines); the wrapper output stays the parity oracle and the post-wrapper read is a state-leak check | a pairing-only mirror on post-wrapper points (r2); an observation hook patched into dashbls | Sol r2 Major 2; dashbls stays the code under test |
| D29 | R1 is stated per group: complete homogeneous formulas for G1 (R1a); Jacobian formulas with H = 0 / R = 0 branches for G2 (R1b), ported with their branches | one "complete formulas" statement for both | `relic_ep2_add.c`, `relic_ep2_dbl.c` (Sol r1 Minor 2) |
| D30 | Element equality is an API op (`BlsElem::eq`) and an ABI-15 script op (`IMPL_EQ` since D73); Group-vs-Group uses blst equality, anything else the transliterated `ep*_cmp` on rebuilt representations; every non-normalizing observable is audited in §3.1 | leaving equality to callers' byte comparisons | `G1Element::operator==` does not normalize (Sol r2 Major 1) |
| D31 | The basic-flag direct-verify invariant is "no arithmetic on the Exotic operand, beyond the exact validity test" (F2: `VerifySecure` may compute with earlier Usable keys first); quirk 45 and A1 say so | "no arithmetic" | Sol r2 Minor 2 |
| D32 | The private security note is unchanged by rev3 (representation and observation-point fixes only) | — | — |
| D33 | `NEG` of an infinite value gives `BASIC` (0, 0, 0) in both groups. `Group::neg` sets the tag to `BASIC` for an infinite input and keeps it otherwise. Sol r3's vector `IMPL_EQ(NEG(G + (−G)), MUL((0, 2), k))` = EQ is a seed and a fixed transition-script step | "neg keeps the tag" (rev3) | `relic_ep_neg.c:38-52` and `relic_ep2_neg.c:39-52` call `set_infty` (Sol r3 Major 1); the rev4 shape probe |
| D34 | **Superseded in part by D59 (Opus r1 M1):** the depth-0 definition and stickiness stand; the step lists are documentation. A trace step is a depth-0 relic call: one that dashbls code makes, `legacy.cpp` included. The step lists include element constructors, `ToNative`/`FromNative` copies, guard `bn_make`s, the map's own calls and its `assert`, and every `CheckRelicErrors` and `NativeVerify` clear point, in Core's exact order | listing only the arithmetic calls (rev3); expanding relic's internal calls | relic's code is sticky until dashbls clears it, so reads at depth 0 see every set. A missing or reordered depth-0 call is what Sol r3 Major 2 found |
| D35 | **Superseded in part by D59 (Opus r1 M1):** (b)'s call log is the observer; (a), (c) and (d) are dropped. Trace fidelity is (a) the final value **and** (b) the executed step sequence equal to the real wrapper's depth-0 call log, with equal codes. The log comes from link-time `--wrap` on every step symbol in the shim build, logging only inside the wrapper window. Also (c) a static check that each step list is derived from its cited lines, its symbols are `U` in the calling object, and its ctor counts match the build (D42, D43), and (d) a recorder selftest. Steps follow the symbol rule (D41); the seams sit in the recipe's `ld -r` step (D56) | the final value only (rev3); hooks patched into dashbls (manager ruling); `-finstrument-functions` on dashbls (it changes Core's objects) | Sol r3 Major 2: a final-boolean match does not prove order. `--wrap` leaves dashbls's objects byte-identical |
| D36 | Port F9. Under basic, `VerifyInsecureAggregated` returns `false` on equal hashes after the argument check and before any relic call; legacy has no such check. `verify_insecure_aggregated`, `VERIFY_AGG`, the step lists and the seeds (equal hashes under both flags) carry it | — | `schemes.cpp:416-431`, found in the rev4 sweep |
| D37 | `DHKeyExchange`'s `operator*(G1Element, PrivateKey)` has no `CheckRelicErrors` (`privatekey.cpp:184-192`), so a code set by its `g1_mul` would outlive the wrapper. The trace records it, and the state-leak check catches it after the op. D8 is unchanged: a nonzero code is a finding | — | found in the rev4 sweep |
| D38 | The rev4 source sweep (§3.5) is part of the design record: every producer row and every trace row, with its source lines and result. A revision that changes a row re-runs it | — | Sol r3 asked for the sweep |
| D39 | The private note is kept current by rev4 | — | §7 embargo ruling |
| D40 | **Superseded in part by D61 (Opus r1 m1):** the `form` field is the `PAIR` flag bit; the step lists are documentation (D59). `AGG` has separate pair and span step lists, including the wrapper constructors: an empty span is one ctor (`return {}`), a non-empty span is `ret`'s ctor, then `CoreMPL::Aggregate`'s, then the adds, and the pair form has only the scheme's ctor. The ABI op gains a `form` field, and pair ops on wrapper-invalid registers are refused | one combined row (rev4) | `bls.cpp:144-176, 222-255` (Sol r4 Major 1) |
| D41 | **Superseded by D60 (Opus r1 M1):** the wrap set comes from `nm`; the rule survives as step-list notation. Symbol rule: a step is an undefined external relic symbol of the calling object, identified by its linked name. Static or inlined dashbls helpers and macros are expanded, block names are notation, and `core_get` is not a step | helper names as steps (rev4); wrapping `core_get` (the recorder needs the real accessor, and it sets no code) | `--wrap` sees only undefined external references (Sol r4 Major 2) |
| D42 | **Superseded by D60 (Opus r1 M1).** Checker (c) runs `nm` on the pinned objects: every step symbol must be `U` in the object that compiles the cited file. Local, defined or missing symbols fail | reviewing helper status by hand | catches the next unexpanded helper mechanically (Sol r4) |
| D43 | **Superseded by D59 (Opus r1 M1).** Constructors that depend on copy elision (`CBLSWrapper`'s move constructor default-initializes `impl`, `bls/bls.h:61-66`) are pinned per build. The step list marks them as elision ctors. Checker (c)'s relocation count is a drift guard, and (b) on the required path scripts fixes which path runs them (revised by D53) | fixing the count from the source (the v23.1.8 build has three G1 sites but two G2 sites for the same source shape) | codegen decides; `set_infty` cannot set the code, so only fidelity (b) depends on it |
| D44 | **Superseded by D59 (Opus r1 M1):** an unwarmed log is harmless. The legacy map's once-per-process constants (`std::call_once`, `legacy.cpp:17-41`) are forced at shim start by one legacy map outside any window, and the shim asserts that both ran (D55); a log that contains them is a shim fault | modelling the once-flag in the trace (other shim entries also map) | keeps the legacy step list state-free |
| D45 | **Superseded by D59 (Opus r1 M1) as a requirement; kept in the step-list documentation.** `HashPubKeys`' `Util::Hash256` calls are steps (`md_map_sh256`: once over all keys, then once per key before `bn_read_bin`) | — | `util.hpp:61-64` inlines `md_map_sh256`; found in the rev5 helper sweep |
| D46 | Negative scalars: only the path for at most 64 bits ends with `g*_neg`; a longer one is reduced by `bn_mod` to a non-negative value first. Negative scalars are outside the wrapper domain, and the engine ports both paths without a certification claim | "a negative scalar ends with `g*_neg`" (rev4) | `relic_pc_exp.c:44-60`; Sol r4's probe (65-bit negative, off-curve: NE) |
| D47 | F8 names only the operations that clear the code; `IsValid`, `SerializeToArray`, `operator==`, the copies and both secret-key products have no `CheckRelicErrors` | rev4's "every operation except…" | Sol r4 Minor 2; the step lists already match |
| D48 | The private note is unchanged by rev5 (step-list and wording fixes only) | — | — |
| D49 | **Superseded in part by D59 (Opus r1 M1):** the trace clauses go; the hash model stays, on the new op `SER_HASH` (D61). `SER` keeps `GetHash`, because Core consumers hash wrappers. Its step list has the cache branch: two `ep*_write_bin` calls when the cache is null and the wrapper is valid, one otherwise. The script model carries a per-register cache value with Core's null sentinel (D58), so (b) and the injection selftest (d) cover the second write | dropping `GetHash` from the op | Sol r5 Major 1; F10 |
| D50 | **Kept as a record (Opus r1 M1, D59).** The cache-clearing writes are F10's list. No other ABI op touches `GetHash` or `cachedHash` inside a window (the rev6 sweep in §3.5) | — | Sol r5 asked for the sweep |
| D51 | Object-level evidence comes only from the build `dash-oracle` links (§3.6, with hashes); the v23.1.8 archive is withdrawn | — | Sol r5 Major 2 |
| D52 | **Superseded by D59 (Opus r1 M1):** the hashes stay as provenance; no checker. Checker (c) reads the exact `ld -r` inputs. It rebuilds the pin with the tree kept, requires the archive sha256 to equal `pins.txt`, and requires the input objects' sha256 to equal the recorded values. `nm` and relocation checks run on those objects and on the final object | trusting an archive found elsewhere | Sol r5 Major 2 |
| D53 | **Superseded in part by D59 (Opus r1 M1):** the scripts stay as seeds; the relocation guard goes. The relocation total per wrapper function is a drift guard only. (b) must run and match both span paths, empty and non-empty, in both groups, and the pair form, including the G1 elision ctor (the §3.3 required path scripts) | the total as proof of path sequences | Sol r5 Minor 1 |
| D54 | F8 says `CheckRelicErrors` clears before a return or a relic-error throw; `CheckValid` throws on `!IsValid()` before its clear point | "before it returns or throws" | Sol r5 Minor 2; `elements.cpp:144-148, 367-371` |
| D55 | **Superseded by D59 (Opus r1 M1).** Before any window opens, the shim asserts that both once-initializers ran: `__wrap_bn_read_str` counts the two constant strings, and each count must be 1 | relying on the warm-up alone | Sol r5's `call_once` note; relic's parameter set-up also calls `bn_read_str`, so the count is by string |
| D56 | **Amended by D60 (Opus r1 M1):** the seams cover the `nm`-derived wrap set. The `--wrap` seams use the recipe's `__wrap_`/`__real_` mechanism on its `ld -r` line, in the plain flavor. BS11a-2 measures their cost (`perf stat -e instructions:u` over the oracle's existing op set), and above 5% it adds a trace flavor built by the same recipe | `-Wl,--wrap` on the final Rust link (rev4–rev5: it cannot reach the localized object); patching dashbls | `build-oracle.sh` builds one localized `ld -r` object; the recipe already wraps Core seams this way (MANIFEST `wrapped`) |
| D57 | The private note is unchanged by rev6 | — | — |
| D58 | The cache model uses Core's sentinel: `GetHash` serializes whenever `cachedHash.IsNull()`, then stores the digest. An all-zero digest stays null and the next `SER` serializes again. Any "cached" flag in the model is derived from `!cachedHash.IsNull()` after the store. No known input produces that digest, so the required scripts stay as they are | a populated flag set by the first `SER` | Sol r6 minor; `bls/bls.h:141-146` |
| D59 | D8 is certified from the real wrapper's call log: the code after every windowed depth-0 call into relic, read without clearing it, plus the post-op read; decode-symbol entries are exempt (D8). A test-only injection selftest proves the log reports a set code. The mirror trace, fidelity (a), (c) and (d), the object-hash checker, elision-ctor pinning and the once-initializer assertion are dropped; the step lists stay as non-normative documentation | keeping the mirror (rev2–rev6.1); wrapping only `CheckRelicErrors` and `fp12_cmp` (Opus's fallback, kept in reserve if the full log fails D56's 5%) | the probe confirms both preconditions at the pin: the code is sticky with `CHECK` off, and a `__wrap_` read does not clear it. The log observes Core itself (Opus r1 M1) |
| D60 | Wrap set = `U` in a dashbls object or Core's `bls` object ∩ `T` in a relic member, minus `core_get`: 83 at the pin, 84 with it (D70). The recipe computes it with `nm` at every build and generates the wrappers from relic's header prototypes; an unparsable prototype or a generated count other than the wrap set's fails the build. A window flag and a thread-local depth counter keep relic's internal cross-object calls out of the log | hand step lists as the symbol source (D41, D42) | `--wrap` redirects every undefined reference in every input; the probe saw 112 internal calls during parameter set-up |
| D61 | ABI 15 on the pin's ABI 14: the reserved int32 carries relic's code; flags bit `0x02` `LOG` appends the call log (symbol names and codes) and `0x04` `PAIR` makes ops 8/9 the pair form; new ops 20 `EQ`, 21 `ADD`, 22 `NEG`, 23 `MUL`, 24 `ISVALID`, 25 `SER_HASH`, and (D73) 26 `IMPL_EQ`, with op 20 `EQ` the wrapper `operator==`; ops 0, 4, 5, 6, 8, 9 and 15 are reused, and op 1 is unchanged. BS11a-2 starts now; A-7 is closed | a new `SER` semantics on op 1 (it would change BS12's corpus silently); waiting for a BS12 commit (D25) | `oracle.h:74`, `:596-660` at `0dd12af0644b` (Opus r1 m1) |
| D62 | A fresh, reset or failed-decode wrapper is Core's default element, `BASIC` (0, 0, 0), on the paths that do not gate it (rev7.1: narrowed by D69): span `AggregateInsecure` (valid result for any non-empty span) and `VerifySecureAggregated` (signature and keys). F11 states it, §6 has `from_impl` and `aggregate_insecure`, and §7/§8 seed it under both flags. Pure Core semantics | `from_value -> Option` only; returning `false` or skipping invalid inputs | `bls.cpp:155-176`, `:233-255`, `:341-358`; `bls/bls.h:95-123` (Opus r1 M2) |
| D63 | Fold order is part of an aggregate's value off the curve: a left fold from the default element in input order; a pair chain equals the span fold; consumers reproduce Core's orders (§6; A-10). Core's parallel aggregator, which combines in completion order, is a documented exclusion | leaving order to callers | relic's `g*_add` is not associative off the curve (Opus r1 m2) |
| D64 | Gated PF0 `instructions:u` rows against dashd for legacy-flag `verify_insecure` and `verify_secure_aggregated` (50 and 400 keys), keys pre-resolved and unresolved; Core is measured through the pinned oracle archive. Route: blst pairing, blst multi-scalar multiplication on all-Usable sums, and `BlsValue::resolve` memoizing classification in the key caches | "reported only" against today's Chia `verify` | CHARTER Performance: never slower than dashd (Opus r1 m3) |
| D65 | A static throw-site table (§3.3) checks D8 over the wrap set's relic call graph and over dashbls's own `RLC_THROW` sites at the pin (D71); the runtime log guards it. The script is committed in BS11a-2 and rerun on pin moves | fuzzing alone | rare inputs; Opus r1 m4 |
| D66 | One dispatch rule: by representation for `ADD`/`NEG`/`MUL`/`AGG`/`IMPL_EQ` (all `Group` → blst; any `Relic` → engine); decode gives `Group` iff Usable; engine results stay `Relic`; only the pairing converts an in-subgroup `Relic` operand to blst after normalization | two statements ("engine result outside the subgroup" vs "stays `Relic`") | Opus r1 m5; both are observationally equal for ADD/NEG/MUL, but they differ in cost and in what the model target asserts |
| D67 | The IES key fix in `dash-pkc` is BSX catalogue item B-23 with upstream unit U30 (Tier 1), so the fork and upstream do not diverge silently | fixing it in the fork only | the bug is present upstream (`ies_ops.rs` at upstream `33fcc59`); Opus r1 m6 |
| D68 | The private note gained a rev7 addendum | — | §7 embargo ruling |
| D69 | Core's wrapper keeps `fValid` and `impl` apart. An invalid wrapper's `impl` is the default element or the receiver's previous element, by operation (F11). `BlsValue` carries both, its equality is the wrapper `operator==` (validity, then element), `invalidate` keeps the element, and the ungated paths read it. ABI 15's `INPLACE` flag lets the oracle reach the kept-element classes, seeded under both flags (§7, §8). BS11a-1 lands the representation | "invalid ⇒ default element" (rev7 F11); a validity-only `Option` | `bls/bls.h:50-123`; `bls.cpp:83-123`, `:144-153`, `:179-231`, `:360-391` (Sol r7 Major) |
| D70 | Wrap-set counts: 84 symbols in the raw intersection (`wrapset.all`), 83 in the wrap set without `core_get`, 79 throw-table roots without the four init-only symbols. The generated-count check uses the 83 | one count for two sets | Sol r7 Minor 1 |
| D71 | dashbls's own `RLC_THROW` sites join the static table: `legacy.cpp:80, 133, 417` and `threshold.cpp:67` sit inside `RLC_CATCH_ANY` and are dead with `CHECK` off; `legacy.cpp:336`, `ep2_map_legacy`'s length check, sees a 32-byte message on every BS11a path. D65 covers relic's call graph from the wrap set plus these sites, and no more | a relic-only table described as complete | a macro expanded in a dashbls object is not a call into relic, so the wrap set cannot log it (Sol r7 Minor 2) |
| D72 | The private note gained a rev7.1 addendum | — | §7 embargo ruling |
| D73 | Op 20 `EQ` is Core's wrapper `operator==` (`fValid == r.fValid && impl == r.impl`); new op 26 `IMPL_EQ` is dashbls `operator==` on the `impl`s, which `BlsElem::eq` ports. Element scripts (transition, §3.1 boundary, fixed vectors) use `IMPL_EQ`; scripts with possibly invalid registers use `EQ`; invalid-register and kept-element scripts use both. `BlsValue`'s equality corresponds to `EQ` | one op for both (a kept-element result would compare equal to its valid receiver) | `bls/bls.h:77-80`; `bls.cpp:179-200` (Sol r8 Major 1) |
| D74 | A throwing `Unserialize` leaves the second `SetBytes`'s result: valid when the other scheme decodes the bytes, invalid with the default element when it rejects them; a stream read that throws first leaves the prior state | "always valid" (rev7.1) | `bls/bls.h:100-123`, `:183-203` (Sol r8 Minor 1) |
| D75 | F11 states `GetHash` as F10 does: `SerializeHash` over the zero serialization, cached under the null sentinel | "zeros" | `bls/bls.h:141-146` (Sol r8 Minor 2) |

## 11. Asks

r0's asks were ruled on by the manager (2026-10-14):

| r0 ask | Ruling | Where |
|---|---|---|
| A-1 ABI number | no BS11a entry; extend BS12's ABI-14 entry at ABI 15 | §7, D13 |
| A-2 BS12 boundary | one script entry (BS12's); the DEC-79 serial verify is BS12's, and BS11a-4 adds the engine branch behind it | §6, §7, D22 |
| A-3 PLAN rows | applied on this branch (BS11a, BS11b, the PLAN:1049 risk row) | docs commit, D24 |
| A-4 A1/EV1 wording | applied on this branch (A1 §1.4.1 and lines 319-320, D58, §4 (h), the BS11a row; EV1 §2.5 and D2; COVERAGE B1/B2) | docs commit, D24 |
| A-5 quirk ID | quirk 45, row added | §8, D23 |
| A-6 scan (h) | optional, folded into slice 4 as non-blocking | §8, §9, D21 |

Open:
- **A-7 (BS12): closed by D61.** ABI 14 at the pin already has the op code room, reserved flag bits and the reserved int32.
- **A-8 (LQ/CH7b, a note):** `BlsCheck` with exactly one key uses `VerifyInsecure`, not secure aggregation (S2).
- **A-9 (owners of node code that hashes BLS wrappers, a note):** Core's wrapper hash is cached at the first call, under the global flag at that moment, and survives a later flag change until a write clears it (F10). Node code that keys maps or sets by a wrapper hash must reproduce this or show that the flag cannot change in between. BS11a's API exposes no wrapper hash; the oracle model covers it (D49).
- **A-10 (BS11b, LQ, a contract note):** off the curve an aggregate depends on its fold order. Aggregate in Core's order (§6, D63): batch-verifier hash order, then `MessageId` order; worker job order; DKG message order. Core's parallel aggregator is not ported.
- **A-11 (owners of the MN-list and quorum key caches, a note):** store keys resolved with `BlsValue::resolve`, so the legacy-flag verify does not reclassify per call (D64).
