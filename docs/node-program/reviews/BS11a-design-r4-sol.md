# BS11a design review, round 4 (narrow)

Target: `839fc8a3c` (`node/bs11a-design`), compared with `40cc6b1b6`. Reference: Dash Core tag `v24.0.0-rc.3` (`da567d7dba`). Repository files were not changed.

## Findings

### Major 1 — `AGG` still omits wrapper constructors

The §3.3 `AGG_PK`/`AGG_SIG` step row says an empty span has **no steps** and a nonempty span starts with the scheme's `aggregated` constructor (`docs/node-program/designs/BS11a-legacy-bls.md:227`). For the span overloads, Core first returns `{}` on an empty span, or constructs `CBLSPublicKey ret` / `CBLSSignature ret` on a nonempty span (`src/bls/bls.cpp:155-169,233-247`). Their `CBLSWrapper` default constructor initializes the `impl` member (`src/bls/bls.h:49-57`); `G1Element()` and `G2Element()` call `g1_set_infty` and `g2_set_infty` (`src/dashbls/include/dashbls/elements.hpp:39-41,82-84`). The nonempty span then enters `CoreMPL::Aggregate`, which constructs a **second** `aggregated` element before the first add (`src/dashbls/src/schemes.cpp:176-191`). The pair overload has only the scheme constructor (`src/bls/bls.cpp:144-148,222-226`). The single combined row therefore cannot match the real wrapper's symbol sequence for either span case. Split pair/span paths and include the return-wrapper constructor, including for an empty span.

### Major 2 — `MapL` lists same-object helpers as loggable steps

The `MapL` row names `SwEnc(t0)`, `SwEnc(t1)` and `MulCof(p,p0)` as steps, but does not spell out their branch-dependent relic calls (`docs/node-program/designs/BS11a-legacy-bls.md:219`). `ep2_sw_encode` and `ep2_mul_cof_b12` are `static` functions in the same `legacy.cpp` object as `ep2_map_legacy` (`src/dashbls/src/legacy.cpp:93,150,306,410-414`). In the existing Core static archive, `nm` shows `ep2_sw_encode` as a local `t` symbol and no `ep2_mul_cof_b12` symbol (inlined); GNU `--wrap` cannot intercept those helper calls. The relic calls *inside* them are external undefined symbols in `libdashbls_la-legacy.o` (for example `ep2_add_projc`, `fp2_mul_integ`, `ep2_mul_basic`) and are interceptable. D35 requires each executed step to match the call log symbol for symbol (`docs/node-program/designs/BS11a-legacy-bls.md:204-205,241,464`). Expand the helper bodies into actual relic-symbol steps, with branches and order, or make the helper names explicit non-log annotations. As written, the claimed step-by-step fidelity check is not defined for legacy maps.

Archive evidence: `nm -A /work/scratch/bs11a/relic/libdashbls.a` prints `libdashbls_la-legacy.o: ... t _ZL13ep2_sw_encode...` and `U ep2_add_projc`, `U ep2_mul_basic`, `U fp2_mul_integ`; it prints no `ep2_mul_cof_b12` symbol.

### Minor — the negative-scalar sweep result is overbroad

The sweep says a negative scalar ends with `g*_neg` (`docs/node-program/designs/BS11a-legacy-bls.md:122,266`). This is true for the ≤64-bit path (`src/dashbls/depends/relic/src/pc/relic_pc_exp.c:44-49,96-101`). For larger negatives, `g*_mul` first computes `bn_mod(_b,b,n)` and passes the resulting nonnegative scalar to `ep*_mul` (`relic_pc_exp.c:52-60,104-112`; `src/dashbls/depends/relic/src/bn/relic_bn_div.c:57-74,78-118`). It does **not** end with `g*_neg`. The off-curve result can differ from multiplying by the magnitude and negating. A scratch probe linked against Core's relic archive printed `bits=65 negative=1 on_curve=0 cmp=2 code=0` (`cmp=2` means NE) for a 65-bit negative scalar and an off-curve G1 point. The row itself says negative scalars are unreachable from the wrapper's byte scalars, so this does not change the stated BS11a input domain; narrow the claim.

### Minor — F8 still overstates which operations clear errors

F8 says every dashbls element operation ends in `CheckRelicErrors`, except copies and the G1 private-key product (`docs/node-program/designs/BS11a-legacy-bls.md:68`). `IsValid`, `SerializeToArray` and `operator==` have no such call (`src/dashbls/src/elements.cpp:135-142,178-203,358-365,400-440`); the G2 private-key product also has none (`src/dashbls/src/privatekey.cpp:197-205`). The §3.3 step lists correctly omit `[clr]` for `ISVALID`, `SER` and `EQ`, so narrow F8 to the operations it actually describes.

## Checks that matched

- **M1 resolved.** `ep_neg` and `ep2_neg` test infinity first and call `set_infty` (`relic_ep_neg.c:38-52`; `relic_ep2_neg.c:39-52`), which sets `(0,0,0), BASIC` (`relic_ep_util.c:43-48`; `relic_ep2_util.c:39-48`). The rev4 table, `Group::neg` rule and fixed vector carry this. Re-running `/work/scratch/bs11a/probe/shape_probe` printed `G1 neg(P+(-P)) coord=1 infty=1 x0=1 y0=1`, the same G2 line, `cmp(neg(G+(-G)), art)=0`, and `fails=0 code=0`.
- **M2's verify order resolved.** Basic `Verify` maps, then constructs and negates the generator, then checks key and signature (`schemes.cpp:135-153`). Legacy `Verify` constructs/negates generator, copies key and signature, then maps (`:658-669`). Basic aggregate verify checks the signature once, then interleaves each key's check, copy and map (`:297-324`); legacy interleaves each key's copy and map (`:683-703`). The rev4 rows at design lines 232-235 reflect these orders.
- **F9 confirmed.** Basic `AggregateVerify` checks duplicate message byte sequences after the argument check and before calling `CoreMPL::AggregateVerify` (`schemes.cpp:416-431`; wrapper at `src/bls/bls.cpp:314-339`). Legacy lacks that distinctness check (`schemes.cpp:683-703`). `VerifySecure` calls virtual `AggregateVerify` with one message (`schemes.cpp:253`). The n=0 argument branch is unreachable through the checked wrapper paths.
- **Sticky code confirmed for the stated build.** With `CHECK` off (`src/dashbls/CMakeLists.txt:65`), `RLC_THROW` sets `core_get()->code` (`relic_err.h:251-265`). The only relic resets found are `core_init` (`relic_core.c:121`) and `err_get_code` (`relic_err.c:101-105`); dashbls clears in `CheckRelicErrors` and `NativeVerify` (`dashbls/src/bls.cpp:81-91`; `schemes.cpp:343-348`). A read after each actual depth-0 relic call therefore catches a deeper code before those clear points.
- **`--wrap` boundary checked.** The actual relic calls sampled in `legacy.o` are undefined external references and can be intercepted by static-link `--wrap`; the local/inlined helper names in Major 2 cannot. This distinction is visible in the archive's `nm` output.
- **Requested sweep samples.** `SubInsecure` does `impl + o.impl.Negate()` (`src/bls/bls.cpp:289-294`). `VerifySecure` serializes keys, sorts, computes weights, then re-decodes/multiplies/adds one sorted key at a time before a virtual aggregate verify (`schemes.cpp:231-254`), as the trace says. The zero-scalar `g*_mul_gen` path writes infinity (`relic_ep_mul.c:715-719`; `relic_ep2_mul.c:358-362`). The aliased DH multiply is supported by G1/G2 multiplication paths that read the input into a precomputation table before writing the result (`relic_ep_mul.c:86-101,742-777`; `relic_ep2_mul.c:62-81,385-420`). The nonnegative wrapper scalar paths match the sweep.

## Verdict

CHANGES-REQUIRED
