# BS11a design review, round 3 (narrow)

Target: `40cc6b1b6`, delta `ed6891e26..40cc6b1b6`. Core source: `~/workspace/dash` at v24.0.0-rc.3; the reviewed source files have no diff from that tag. The independent probe is `lemma_probe.cpp` in this directory, linked to the Core relic/dashbls archives in `/work/scratch/bs11a/relic/`.

## Findings

### Major 1 — `NEG` does not preserve the coord tag of an infinity

The producer table in BS11a §3.1 says negation keeps the tag. In both groups, relic tests `z == 0` and calls `set_infty`, which writes `(0,0,0), BASIC`, before the finite negation path. Core evidence: `src/dashbls/depends/relic/src/ep/relic_ep_neg.c:38-52`, `src/dashbls/depends/relic/src/epx/relic_ep2_neg.c:39-52`, `src/dashbls/depends/relic/src/ep/relic_ep_util.c:39-48`, and `src/dashbls/depends/relic/src/epx/relic_ep2_util.c:39-48`. The wrapper `Negate()` calls these functions (`src/dashbls/src/elements.cpp:153-159, 385-390`).

This is observable in G1. `G + (−G)` is a PROJC infinity `(0,y≠0,0)`; `NEG` of that result is a BASIC infinity `(0,0,0)`. `ep_cmp` compares a lone infinity against a finite value using the raw coordinates (`src/dashbls/depends/relic/src/ep/relic_ep_cmp.c:42-90`). Probe output against the finite `(0,0,1)` artifact:

```text
G1 add inverse: coord=2 x0=1 y0=0 z0=1; neg: coord=1 x0=1 y0=1 z0=1; cmp artifact: before=2 after=0
```

Here `2` is `RLC_NE`, `0` is `RLC_EQ`. Thus `EQ(NEG(G + (−G)), artifact)` differs if `Group::neg` merely retains the tag. Rev3's r2 M1 is improved but not resolved. Add the infinity reset to the table, implementation rule, model target and fixed transition script.

### Major 2 — the `VERIFY` and `VERIFY_AGG` raw traces still reorder relic calls

BS11a §3.3 describes the basic `VERIFY` trace as key/signature validity checks followed by the message map, generator and negation. The actual `CoreMPL::Verify` does `G2Element::FromMessage` first, then `Generator().Negate()`, then `pubkey.IsValid()` and `signature.IsValid()` (`src/dashbls/src/schemes.cpp:135-153`). `FromMessage` calls `ep2_map_dst`, checks relic errors, and asserts `IsValid()` (`src/dashbls/src/elements.cpp:334-347`). The map therefore runs even when an Exotic key makes basic verify return false. Stopping the trace at that validity check would omit a real relic call and could miss the first error code; matching the final boolean does not establish trace fidelity.

The `VERIFY_AGG` row also presents basic validity checks followed by the per-message maps. Core instead checks the signature once, then **interleaves** each key's validity check with that key's map (`src/dashbls/src/schemes.cpp:307-323`). This matters when a later key fails validity. Correct the trace order to the exact wrapper paths, including the map's own calls, before relying on D8's per-call error observation. Rev3's r2 M2 remains unresolved.

## Checks that matched

- **Scale and validity:** `g1_is_valid` uses `ep_psi`, addition, `g1_on_curve`, and `g1_cmp` (`src/dashbls/depends/relic/src/pc/relic_pc_util.c:67-112`); `g2_is_valid` uses Frobenius, addition, `g2_on_curve`, and `g2_cmp` (`:137-213`). Curve tests normalize (`src/dashbls/depends/relic/src/ep/relic_ep_util.c:178-190`, `src/dashbls/depends/relic/src/epx/relic_ep2_util.c:172-186`). The comparison paths either cross-multiply or normalize (`src/dashbls/depends/relic/src/ep/relic_ep_cmp.c:38-90`, `src/dashbls/depends/relic/src/epx/relic_ep2_cmp.c:39-72`). The probe found no validity or EQ divergence across 80 Group-plus-Exotic additions using different nonzero Group scales, in both operand orders and groups: `Group+Exotic scale, both orders, EQ and is_valid: mismatches=0/80`.
- **G2 BASIC/non-BASIC comparison:** `ep2_cmp` normalizes both when either is BASIC (`src/dashbls/depends/relic/src/epx/relic_ep2_cmp.c:54-72`). The probe compared a BASIC point with the same point tagged PROJC with `z=1` in both orders; both returned `RLC_EQ`.
- **G2 mixed/general rescaling:** the mixed path is selected by `q->coord == BASIC` (`src/dashbls/depends/relic/src/epx/relic_ep2_add.c:259-261`). The general formulas have doubled `R`, `(2H)^2`, and `z3=2z1z2H` (`:288-330`), while mixed uses `z3=z1H` (`:161-210`). The probe used the same finite point as a BASIC and PROJC second operand and found `general = (4·mixed.x, 8·mixed.y, 2·mixed.z)` exactly, with equal PROJC tags.
- **G2 add with BASIC infinity:** `ep2_add_projc` checks the first infinity before the second and copies the other operand (`src/dashbls/depends/relic/src/epx/relic_ep2_add.c:390-407`). Probe: BASIC infinity plus PROJC infinity has PROJC tag; the reverse has BASIC tag. The table's copy rule is right, although for two infinities it should specify the first-operand check to avoid ambiguity.
- **Group vs Group EQ:** for finite G1 PROJC points, `ep_cmp` cross-multiplies each operand's scale (`src/dashbls/depends/relic/src/ep/relic_ep_cmp.c:53-90`); G2 PROJC comparison does likewise (`src/dashbls/depends/relic/src/epx/relic_ep2_cmp.c:54-64`). Both-infinity pairs return equal first. The probe returned `RLC_EQ` for the same finite Group point at scales 1, 2 and 3 in both groups. Using blst equality for Group-vs-Group is supported for these all-Usable values.
- **Producer audit:** decode, generator, scalar multiplication, non-infinite addition, and addition with an infinity agree with the stated table. The `NEG` infinity branch above is the missing producer case. The `AGG_*` accumulator starts at BASIC infinity and uses in-place adds (`src/dashbls/src/schemes.cpp:176-192`); `DH` copies the key and multiplies in place (`src/dashbls/src/privatekey.cpp:184-192`). Rev3's narrower direct-verify wording (r2 m2) agrees with `CoreMPL::VerifySecure`'s per-key re-decode, multiply and add (`src/dashbls/src/schemes.cpp:231-253`): earlier Usable keys may be processed before an Exotic key throws.

## Verdict

CHANGES-REQUIRED
