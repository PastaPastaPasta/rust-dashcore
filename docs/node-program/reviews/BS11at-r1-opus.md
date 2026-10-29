# BS11a-t BLS decode types, review round 1 (Opus)

| | |
|---|---|
| Reviewer | independent Opus 5.5 |
| Date | 2026-10-08 |
| Under review | base-sdk `node/bsdk-bs11at-bls-types` at `70fb331` (6 commits on `33fcc59`), worktree `~/workspace/bsdk-wt/bs11at-bls-types` |
| Context | A1 §1.4.1, reviews/A1r-r1-opus.md B-1, DEC-24 D2, PLAN BS11a-t / BS11a |
| Core source | `~/workspace/dash` at `v24.0.0-rc.3` (0364ed626e) and `v23.1.8` (728f505583): `src/bls/bls.{h,cpp}`, `src/dashbls/src/{elements,privatekey,schemes,bls}.cpp`, `src/dashbls/depends/relic/` |
| Core builds | `~/workspace/dash-wt/oracle-shim-v24.0` and `-v23.1` (read only; tag + 5 shim commits, none touching `src/bls` or `src/dashbls`; relic configured `ARITH=GMP`, `QUIET`, no `CHECK`, no `DEBUG`) |
| Scratch | `/work/scratch/bs11at-review/` (my probe `rprobe.cpp`, generator `rgen.py`/`rtors.py`, harness `src/pkgs/pkc/examples/review_diff.rs` in an exported copy, logs) |

## Verdict: APPROVE-WITH-NITS

**The legacy rule may replace a relic port: yes** (decode outcomes, canonical bytes, re-encoding in either scheme, and equality; conditions in §8).

I derived the legacy G1 and G2 decode from relic's and dashbls' source as Core builds them, without looking at the author's argument first. I reached the author's rule exactly, with a case proof for every 48- or 96-byte input (§2). Four independent checks against Core's real `CBLSWrapper` agree:
- my own probe, generator and evaluator;
- 2,176 boundary inputs;
- 300,000 structured random inputs;
- 3,000 torsion-stress inputs.

v23.1.8 and v24.0.0-rc.3 agree with each other on every input, and the Rust types match Core on every input, outcome and serialization (§3).

The eager and lazy wrappers follow `bls.h` transition by transition (§4). The single unmodelled behaviour is a stale `obj` after re-`Unserialize` into a used lazy wrapper. I reproduced it in Core, but no decode site I checked reuses a wrapper (nit N1).

Secret-key handling is constant time where it matters (§5). The API fits BS6a, CS0 and BS4a, with small adoption frictions (§6). The corpus provenance is sound (§7).

Counts: blocker 0 · major 0 · minor 4 · nit 8.

## 1. What I read and ran

- **Read:**
  - all of the branch: `scheme_ctx.rs`, `wrap_{codec,value,eager,lazy}.rs`, `tests/bls_wrapper.rs`, the generator, the probe, the CodeQL exemption and the CHANGELOG;
  - Core's `bls.h` (both pins). Only `#include` changes, cppcheck comments and v24's removal of the `#ifndef BUILD_BITCOIN_INTERNAL` guard around the lazy wrapper separate them; none of these changes behaviour;
  - dashbls `elements.cpp` and `privatekey.cpp`, identical in both pins;
  - relic (identical in both pins): `relic_err.h`, `relic_fp_util.c`, `relic_fp_srt.c`, `relic_fpx_srt.c`, `relic_fpx_util.c`, `relic_ep_util.c`, `relic_ep_pck.c`, `relic_ep2_util.c`, `relic_ep2_pck.c`, `relic_ep_cmp.c`, `relic_ep2_cmp.c` and `relic_pc_util.c`;
  - dashbls `configure.ac` and Core's `configure.ac` (the subdir arguments);
  - blst 0.3.17 `e1.c`, `e2.c` and `no_asm.h` (the version base-sdk's lock pins).
- **Ran:**
  - `cargo test --release -p dash-pkc --features full --test bls_wrapper`: 37 pass, 1 ignored;
  - `--lib`: 444 pass;
  - `cargo clippy -p dash-pkc --all-targets` under `RUSTFLAGS=-D warnings` with `--features full`, with `--no-default-features`, and with `--no-default-features --features bls`: clean.
  - All of this ran on an exported copy (`git archive 70fb331`), so the author's worktree was not touched.
- **My differential:**
  - **Probe** (`rprobe.cpp`). It links Core's `libbitcoin_*` and `libdashbls`/`librelic` from both shim trees. For each input it prints:
    - Core's eager `Unserialize` outcome under each flag (T/I/V, with both serializations);
    - the raw per-scheme predicate, `SetBytes` + `IsValid` + `CheckMalleable`.
  - **Rule evaluator** (`rgen.py rule`). It transcribes the derived rule into pure-Python field and curve arithmetic, with no blst, so it tests the rule itself rather than the implementation.
  - **Rust harness** (`review_diff.rs`). It runs `BlsEager::decode` under both flags plus `BlsValue::from_canonical` for both schemes, and compares outcome strings byte for byte.

## 2. The legacy rule, derived from relic as Core builds it

### 2.1 Build facts the derivation uses

- **`CHECK` is undefined.** dashbls `configure.ac` defaults `--enable-check` to `no`, and Core's `configure.ac:2081` passes nothing that enables it. The shim trees' `relic_conf.h` confirms this.
- **So `RLC_THROW` is `core_get()->code = RLC_ERR;`** (`relic_err.h:261`, `QUIET`). Control continues after every throw, and `RLC_TRY`/`RLC_CATCH_ANY` reduce to `if (1)`/`if (0)`.
- **Legacy errors are swallowed and cleared.** The legacy path ends in `BLS::CheckRelicErrors(false)` (`elements.cpp:85`, `:310`), which resets `code` and does not throw. Basic passes `true` and throws on any set code.
- **`FP_RDC = MONTY`.** Field elements are stored as `a·R mod p`, with R = 2^384 for both `WSIZE` 64 (6 digits) and `WSIZE` 32 (12 digits).
- **u² = −1** (`FP_QNRES`). On Darwin `FP_QNRES` is not defined, but relic's computed QNR for p ≡ 3 (mod 4) is −1, so the field is the same.
- **`MULTI = PTHREAD`**: relic's context, including `code`, is per thread.
- **The decode path is identical in both pins.** `elements.cpp`, `privatekey.cpp`, `bls.cpp`'s `CheckRelicErrors` and all of relic are byte-identical between v23.1.8 and v24.0.0-rc.3. The dashbls diff touches only `legacy.cpp` (hash-to-curve init), `schemes.cpp`/`threshold.cpp` (RAII for `bn_t`), bindings and CI.

### 2.2 relic facts (each read in source)

- **F1. `fp_read_bin` leaves its output untouched on failure** (`relic_fp_util.c:230`). A value ≥ p throws, and `a` is **not written**. The `G1Element()`/`G2Element()` constructors call `g1_set_infty`/`g2_set_infty` (`elements.hpp:39-40`, `:82-83`), which zero x, y and z. A coordinate ≥ p therefore leaves that coordinate 0 (for G2, that one component).
- **F2. `ep_read_bin`/`ep2_read_bin` and the sign bit** (`relic_ep_util.c:237`, `relic_ep2_util.c:246`):
  - They set z = 1 and read x.
  - y is set to the raw Montgomery digit s, the sign bit: for G2 that is y.c0 = s and y.c1 = 0.
  - They call `ep_upk`/`ep2_upk` and **ignore its result**.
  - Then `ep_on_curve` fails with a throw that only sets the code.
- **F3. `ep_upk`/`ep2_upk`** (`relic_ep_pck.c:77`, `relic_ep2_pck.c:73`) compute t = √(x³ + b) with `fp_srt`/`fp2_srt`.
  - **If t exists**, they set y = ±t so that sign(y) = s, and z = 1:
    - G1: sign(y) = [y > (p−1)/2];
    - G2 (lexicographic): sign(y.c1) if y.c1 ≠ 0, else sign(y.c0).
  - **If t does not exist**, they return 0 and leave the point unchanged, so an off-curve point keeps (x, s·R⁻¹, 1), and for G2 (x, (s·R⁻¹, 0), 1).
- **F4. Both square roots are exact.**
  - `fp_srt` (`relic_fp_srt.c:38`): since p ≡ 3 (mod 8), it computes a^((p+1)/4) and checks the square.
  - `fp2_srt` (`relic_fpx_srt.c:39`) uses the norm method. With a₁ = 0 it takes the real root, or the imaginary root via −a₀. Otherwise it checks that N(a) = a₀² + a₁² is a square and takes c₀ = √((a₀ ± √N)/2) and c₁ = a₁/(2c₀).
  - Both return 1 exactly for squares, and c₀ ≠ 0 whenever a₁ ≠ 0.
- **F5. `ep_pck` and `ep2_pck` disagree on the sign** (`relic_ep_pck.c:38`, `relic_ep2_pck.c:39`). The G1 sign is [std(y) > (p−1)/2]. The G2 sign is **[std(y.c1) > (p−1)/2] only**; there is no fallback to c0. This asymmetry with F3 is the "sign ∧ y.c1 = 0" clause.
- **F6. The identity test in `SetBytes`.** `impl == ImplType()` uses `ep_cmp`/`ep2_cmp` against the zeroed default (`relic_ep_cmp.c:38`, `relic_ep2_cmp.c:39`). With both points `BASIC` it compares raw x and y, so a decoded point (z = 1) "is the identity" iff x = 0 ∧ y = 0.
- **F7. Numeric facts** (computed in `facts.py`):
  - R⁻¹ mod p = 0x14fec701e8fb0ce9ed… > (p−1)/2;
  - #E(Fp) = h₁·r is odd, and h₂ is odd, so no on-curve point has y = 0 in G1 or G2;
  - N(4+4u) = 32 is a non-residue (p ≡ 3 mod 8, so 2 is a non-residue), so G2 x = 0 is off the curve;
  - p < 2^381 and p's top byte is 0x1a, so a value below p never has bits 0x20/0x40/0x80 of its top byte set.

### 2.3 G1, legacy: canonical ⇔ b ≠ 0 ∧ b₀ & 0x60 = 0 ∧ x < p

Here x is the value of b with bit 0x80 of b₀ cleared. Let s = b₀ >> 7. All-zero input is reset by `SetBytes` before decoding.

1. **b₀ & 0xc0 = 0xc0.** dashbls' infinity branch (`elements.cpp:46-55`) either returns the identity (exactly `c0 00…`), which `SetBytes` resets, or throws, which also resets. Either way the object serializes as zeros ≠ b, so the bytes are not canonical, and this case violates b₀ & 0x60 = 0.
2. **Otherwise dashbls masks b₀ to 0x1f** and passes prefix 02 or 03 (s), and relic reads x' = (b with the top 3 bits of b₀ cleared).
   - **x' ≥ p:** x stays 0 (F1). Then x³ + 4 = 4 is a square, so the point is (0, ±2), and it serializes with x = 0 ≠ x'.
   - **x' < p, on the curve:** y ≠ 0 (F7) and sign(y) = s (F3), so `ep_pck` re-encodes s.
   - **x' < p, off the curve:** y = s·R⁻¹ (F3). `ep_pck` encodes [s·R⁻¹ > (p−1)/2] = s (F7).
   - **The identity test (F6) never fires.** x = 0 is on the curve with y = ±2 ≠ 0, and an off-curve x is nonzero.
3. **Re-serialization** (`SerializeToArray(true)`, `elements.cpp:178`) writes x' (whose 0x60 bits are clear, F7) with 0x80 = s. This equals b iff b₀ & 0x60 = 0 and x' < p. ∎

So every x < p round-trips with either sign bit. The object is the blst point when x³ + 4 is a square; otherwise it is relic's (x, s·R⁻¹, 1), which no blst type can hold (`Exotic`). This includes x = 0 with s = 1, the order-3 point (0, p−2), which blst flags `POINT_NOT_IN_GROUP` (`e1.c:289`) and the code maps to `Exotic` (`wrap_codec.rs:180`).

### 2.4 G2, legacy: canonical ⇔ b ≠ 0 ∧ b₀ & 0x60 = 0 ∧ b₄₈ & 0xe0 = 0 ∧ c₀, c₁ < p ∧ [(on ∧ ¬(s ∧ y.c1 = 0)) ∨ (off ∧ ¬s)]

The layout is c₀ = b[0..48] with bit 0x80 cleared, c₁ = b[48..96] and s = b₀ >> 7. x = c₀ + c₁u, and "on" means x³ + 4(1+u) is a square in Fp2.

1. **b₀ & 0xc0 = 0xc0 always throws.** The legacy branch does not mask before the infinity test, so `fZerosOnly` sees buffer[1] = b₀ ≠ 0 (`elements.cpp:262-283`). Even the canonical `c0 00…` throws.
2. **Otherwise dashbls clears only 0x80** (`:286-288`). The 0x60 bits of b₀ and the 0xe0 bits of b₄₈ stay in the coordinates.
   - Those bits make the coordinate ≥ 2^381 > p, so `fp_read_bin` leaves that component 0 (F1, `relic_fpx_util.c:75`).
   - Re-serialization writes every component below p, so any component ≥ p (stray bits included) mismatches. Clauses 2–4 of the rule are this case.
3. **On the curve** (F3, F4): y has lexicographic sign s, and y ≠ 0 (F7). `ep2_pck` encodes sign(y.c1) (F5).
   - If y.c1 ≠ 0, that is s, so the bytes round-trip.
   - If y.c1 = 0, it encodes 0, so the bytes round-trip iff s = 0.
4. **Off the curve:** y = (s·R⁻¹, 0) and `ep2_pck` encodes 0, so the bytes round-trip iff s = 0.
5. **The identity test (F6)** fires only when x = 0 and y = 0. x = 0 is off the curve (F7), so that needs s = 0, which with x = 0 means either b = 0 (handled first) or a component was replaced (already not canonical). ∎

### 2.5 Basic, which the fallback step and basic fields need

**G1:** canonical ⇔ b₀ & 0xc0 = 0x80 ∧ x' ≠ 0 ∧ x' < p ∧ on the curve ∧ `g1_is_valid` (`elements.cpp:64-81`).

**G2:** canonical ⇔ the same conditions, plus b₄₈ & 0xe0 = 0, `g2_is_valid`, and ¬(sign ∧ y.c1 = 0) (F5 again, through `SerializeToArray(false)` at `:400`).

**The implementation** (`g1_decode_basic`, `wrap_codec.rs:156`; `g2_decode_basic`, `:190`) calls blst `uncompress`, which matches relic on every point:
- the same top-3-bit mask on the first half;
- an exact x < p check on both halves (`e1.c:242-247`, `e2.c:285-296`);
- the same lexicographic sign (`no_asm.h:538-552`);
- `NOT_IN_GROUP` for G1 x = 0, where dashbls throws "can't have only zeros";
- for G2 x = 0, `NOT_ON_CURVE` (F7), where dashbls throws as well.

After `uncompress`, the implementation runs blst's `in_g1`/`in_g2`.

**Subgroup checks.** relic tests membership with Bowe's ψ-based identities (`relic_pc_util.c:67`, `:137`) and blst with Scott's. Both are proven exact for BLS12-381. My torsion stress (§3) exercises every ℓ-power part of h₁ and h₂, pairs of parts, random subsets and the full cofactor, each with and without a subgroup component. The two libraries agree everywhere.

### 2.6 Secret keys

`PrivateKey::FromBytes(b, modOrder)` (`privatekey.cpp:52`): modOrder reduces mod r. Otherwise b > r throws, and r stays unreduced. `SetBytes` resets k = 0.

So legacy (modOrder) is canonical ⇔ 0 < v < r, and basic ⇔ 0 < v ≤ r. The implementation (`wrap_codec.rs:292-299`) is exactly this.

### 2.7 What the equivalence assumes

1. **relic's field arithmetic is exact,** with fully reduced internal representations at `fp_cmp`. If it were not, Core's own builds (GMP and C backends, 32- and 64-bit) would disagree with one another.
2. **blst 0.3.17 is correct.**
3. **relic's `code` is `RLC_OK` when a basic decode starts.** Only the basic path cares; the legacy path ignores and clears it. `code` is per thread. Every dashbls entry point I traced either ends in `CheckRelicErrors` or resets `code` (`NativeVerify`, `schemes.cpp:340-348`), and `Verify` returns before arithmetic when a key or signature is not `IsValid` (`:143-148`). I found no path that leaves it set.
4. **relic is built with `CHECK` off.** This holds for every Core release build. A user passing `--enable-check` to Core's `configure` would reach dashbls through `AC_CONFIG_SUBDIRS` and change legacy outcomes, but that node would diverge from the network anyway.

Nothing in §2.3–2.6 depends on the word size, the GMP or C arithmetic backend, or the OS, so the rule holds on all six release builds.

### 2.8 What the rule gives up: nothing

The canonical bytes determine relic's internal point exactly:
- **on the curve:** the blst point;
- **G1 off the curve:** (x, s·R⁻¹, 1);
- **G2 off the curve:** (x, 0, 1).

So `BlsPoint::Exotic`'s lack of coordinates loses no information. If BS11a ever needs relic's arithmetic on Exotic points, the decoder does not change.

## 3. Differential results (mine)

| Set | Inputs | v24 = v23.1.8 | Rule (pure Python) vs Core | Rust vs Core (outcome + both serializations) |
|---|---|---|---|---|
| Boundaries (`rgen.py gen 7 0`) | 2,176 (pk 660, sig 1,504, sk 12) | yes | **0 mismatches** | **0 mismatches** |
| Structured random (`gen 20261008 300000`) | 300,000 | yes | **0 mismatches**: the legacy predicate on all 300,000, and the basic predicate for pk/sk. Pure-Python G2 subgroup checks were too slow for 300k; the G2 basic predicate is covered by the edge set, the torsion set and the Rust column | **0 mismatches** |
| Torsion stress (`rtors.py 99 600 150`) | 3,000 | yes | (covered by Rust column) | **0 mismatches** |

**Boundaries** cover:
- **G1:** x ∈ {0, 1, 2, 3, (p−1)/2, (p+1)/2, p−2, p−1, p, p+1, 2^380, 2^381−1}, subgroup points, non-subgroup points, off-curve points, and 15 torsion classes (5 single ℓ-parts and 10 pairs, ± a subgroup component), each under all 8 combinations of the 0x80/0x40/0x20 bits.
- **G2:** c₀ × c₁ over {0, 1, p−1, p, p+1, 2^381−1, random} in both layouts and both signs. Point classes: subgroup, non-subgroup, off-curve, y ∈ Fp, y purely imaginary, and torsion (each ℓ, two pairs, the 448-bit factor, ± a subgroup component). Each class also gets b₀ stray bits {0x20, 0x40, 0x60, 0xc0} and b₄₈ bits {0x20, 0x40, 0x80, 0xe0}, plus infinity variants.
- **sk:** 0, 1, 2, r−2, r−1, r, r+1, 2r−1, 2r, 2r+1, 2^255 and 2^256−1.

**Structured random** (pk 120,296 · sig 134,960 · sk 44,744) draws from four sources:
- on-curve x;
- off-curve x;
- x ≥ p;
- unconstrained 381-bit x.

These are combined with random stray-bit patterns, both G2 layouts, b₄₈ top bits, y ∈ Fp solutions for G2, and sk values near r. Core's outcomes on this set were T on 112,665 sig inputs and V on 22,295.

**Torsion stress:** random subsets of the cofactors' prime-power parts (G1 600 points, G2 150), 70% with a subgroup component, each with both signs in both layouts.

Two G2 inputs came out canonical in **both** schemes. Both are basic-encoded subgroup points with sign 0 whose swapped coordinates lie on the curve, which is A1's existing row "basic-canonical, sign 0", so no new class.

## 4. The wrappers against Core's `bls.h`

**Eager** (`wrap_eager.rs:64-76`) follows `bls.h:184-203` exactly:
- zero input → INVALID (`SetBytes` resets it, and the zeros round-trip);
- canonical under F → VALID;
- canonical under ¬F → INVALID;
- otherwise → `BlsDecodeError`.

The fallback uses `is_canonical`, which skips the subgroup classification. That is correct, since the outcome is INVALID either way. Re-encoding takes an explicit scheme, which is right: Core's argument-free `Serialize` reads the global flag, not the decode scheme.

**Lazy** (`wrap_lazy.rs`), compared transition by transition:

| Core (`bls.h`) | Rust | Match |
|---|---|---|
| ctor: `bufLegacyScheme` = global; `bufValid` = `objInitialized` = false | `new(scheme)` | yes |
| `Unserialize(s, X)`:466-475 | `from_bytes(bytes, source)`: `buf_valid` = any nonzero | yes (fresh object only, N1) |
| `Set`:483-491 | `set` / `from_value` | yes, including `Set` of an invalid object (`is_default` is false) |
| `Get`:492-512: returns obj if initialized; invalid if `!bufValid`; `SetBytes` + `IsValid` + `CheckMalleable`, failure → `bufValid` = false | `get` (`:76-90`) | yes. A buffer that is current but zero, after `ser:X` on an uninitialized object, fails `from_canonical` the same way |
| `Serialize(s, X)`:446-458: zeros for the default object; re-encode `obj` (invalid → zeros) and adopt X when `!bufValid ∨ X ≠ buf` | `serialize` (`:63-73`) | yes |
| `GetHash`:542-559, hash cached | `hash` (`:94-97`) via `serialize(scheme)`, no cache | yes. Core's cache is invalidated on every `vecBytes` change, and a failed `Get` leaves `!bufValid`, which forces a refill, so the cache is never stale when read |
| `SetLegacy`/`IsLegacy` | `set_scheme`/`scheme` | yes |
| `operator=`:419-437 | `Clone` | yes (bytes only while `bufValid`; obj only while initialized) |
| `operator==`:514-535 | `PartialEq` (`:221-248`) | yes. The lock order is by address, as `std::scoped_lock` avoids deadlock |

The corpus scripts (every op sequence up to length 2 plus the trap paths) exercise all of this against Core, and they pass.

## 5. Secret keys: constant time and hygiene

- **Range checks are constant time.** `ct_lt` (`wrap_codec.rs:106`) is a branch-free borrow chain and is correct: (x − y − borrow) as u16 has bit 8 set iff it is negative. `ct_eq` handles the zero check and equality (`wrap_value.rs:149`). `is_zero` folds with OR.
- **Only validity is revealed.** Branching on the outcome reveals validity, which Core makes public anyway.
- **Bytes are wiped, redacted and kept out of hashes.** `Valid` zeroizes them on drop (`:129`), `Debug` prints `[redacted]`, and `Hash` writes nothing for secret kinds (`:139`).
- **Leftover copies:** see nit N4.

## 6. API shape for consumers

- **DEC-24 D2 is satisfied.** `BlsCtx`, `BlsSchemeKind` and `BlsSchemeSource` live in `dash-pkc` outside the `bls` feature, so BS4a's `ProtoCtx` and CS0 can name them without blst.
- **BS6a has what it needs:**
  - `BlsEager::decode(&[u8; N], BlsSchemeSource)` → `Result`, with the error mapped to `bad-*-payload`;
  - `to_bytes(scheme)` for `SerializeHash(qc)` (zeros for INVALID);
  - `is_valid()` for `IsNull`;
  - `BlsLazy::to_bytes()` (raw bytes before any `get`) for the SML entry hash.
  - The types are `Unencodable`, which is right: encoding needs a scheme, so BS6a's codecs take one.
- **CS0** needs two edits, not one (nit N2).
- **BS4a** adds its own `pkgs/pkc/src/bls/scheme_ctx.rs` (commit 65f1d53), so there is an add/add conflict (minor M1).
- **Governance and CoinJoin `vchSig`** (A1 per-field table: `SetBytes(basic)` with no round trip): not offered (minor M3).

## 7. Corpus provenance

- **The outcomes come straight from Core.** `probe.cpp` calls Core's real `CBLSWrapper::Unserialize`, `CBLSLazyWrapper` methods and `operator==`. It links Core's static libraries, and the relic build matches Core's release configuration.
- **The header is accurate where it can be checked.** It records the seed, the generator, both tag hashes and the requirement that two probes agree before the corpus is written (`gen.py:run_probes`). The hashes match the tags.
- **Inputs and outcomes are separated.** Inputs come from an independent Python BLS12-381 implementation; outcomes come only from Core.
- **The classes are thorough:** every ℓ-power torsion part of both cofactors, y ∈ Fp and purely imaginary y, exact boundaries, every stray-bit pattern, and lazy scripts over all op pairs.
- **Small precision gaps:** see nit N7.

## 8. Findings

### Minor

**M1. Add/add conflict with BS4a on `pkgs/pkc/src/bls/scheme_ctx.rs`.**
- **What happens:** BS4a's 65f1d53 creates the same file with a smaller `BlsCtx` (`legacy: bool`, `BASIC`/`LEGACY`/`from_legacy`/`is_legacy`). Whichever branch merges second into `node/base-sdk` hits a conflict. A careless resolution that keeps BS4a's file drops `BlsSchemeKind`/`BlsSchemeSource` and breaks this branch.
- **Fix:** resolve by taking BS11a-t's file, which is a superset: BS4a's uses (`BlsCtx::BASIC`, `ProtoCtx::new`) compile unchanged. Better still, rebase BS4a onto BS11a-t's first commit (6fe1011). The manager should sequence this.

**M2. The `BlsLazy` lock is a pure spin lock held across decompression** (`wrap_lazy.rs:21`, `:34`, `:153-155`; `Cargo.toml` `spin` with `spin_mutex` only).
- **What happens:** the first `get()` holds the lock through blst `uncompress` and `in_g2` (tens of µs to over 100 µs for G2). Every other thread touching the object, including `to_bytes`, `hash` and `==`, burns CPU meanwhile. If the holder is preempted, waiters spin for a whole time slice.
- **Where it bites:** Core uses `std::mutex`. A node verifying recovered sigs and ISDLOCKs from worker pools (M7) can share objects across threads.
- **Fix:** under `std`, use `std::sync::Mutex`; or decode outside the lock and publish with `OnceLock`, then mutate `buf_valid` under a short lock. Keep `spin` for `no_std`.
- **Correctness is unaffected.**

**M3. No constructor for Core's round-trip-free `SetBytes(bytes, basic)`.**
- **Where it is needed:** A1's per-field table needs it for governance object and vote `vchSig` and CoinJoin `dsq`/`dstx` `vchSig`: no round trip, no fallback, never throws.
- **How it differs from canonical:** only for G2 encodings with the sign bit set, y.c1 = 0, that pass the subgroup check. Whether any such subgroup point exists is open; the heuristic count is about r/p ≈ 2^-126, so probably none.
- **Fix:** before BS6a or BS8 types those fields, add `BlsValue::from_set_bytes(bytes, scheme)` (or similar). Alternatively, document that `from_canonical(…, Basic)` is equivalent, with a proof that no G2 subgroup point has y ∈ Fp or a probe test. The author's own notes already flag this. This can be a BS6a item instead, but it should be tracked.

**M4. A1 and PLAN must be updated before merge, since this branch moves BS11a's core into BS11a-t.**
- **PLAN:**
  - BS11a-t's row says "the four-step wrapper algorithm for the **basic** scheme". The branch implements legacy too.
  - BS11a's row still reads "pure-Rust port of relic's `ep_read_bin`/`ep2_read_bin`/`fp_srt`", with design gate D.
  - Fix: if the manager accepts §2 as the gate-D evidence, re-scope BS11a to its remainder (§9).
- **A1 §1.4.1:**
  - (a) Replace "Legacy decompression is a pure-Rust port of relic's…" with the rule and a pointer to this review's §2.
  - (b) Add the outcome-table row "G2 legacy, sign bit set, on the curve with y.c1 = 0: THROW under both flags". The current rows "G2 `100`, random: VALID if on curve" and "G2, legacy-canonical" omit it.
  - (c) Extend the "Encoding flags" bullet. relic's `ep2_pck` takes the sign from y.c1 only, so basic G2 with the sign bit set and y.c1 = 0 is not canonical either.
  - (d) Record the type names: `BlsSchemeKind`, `BlsSchemeSource`, `BlsPoint`, `BlsWire`, and the aliases (N5).

### Nits

**N1. Lazy re-`Unserialize` keeps a stale `obj` in Core; Rust cannot express it.**
- **The Core behaviour:** in `bls.h:466-475`, `Unserialize` does not reset `obj`. After a failed `Get()` whose `SetBytes` produced a valid but non-canonical object, a second `Unserialize` into the same wrapper followed by `Serialize(s, ¬X)` emits that stale object's bytes, not zeros.
- **Reproduced:** `stale.cpp` in scratch, a G1 subgroup point with stray 0x20 under legacy. The reused wrapper writes `8ce3d4…`, a fresh one zeros.
- **Why it does not matter today:** `BlsLazy` has no in-place unserialize, so it always behaves like a fresh object. The decode sites I checked (vector unserialize via `emplace_back`, the DMN state diff) construct fresh wrappers.
- **Fix:** document this on `BlsLazy`, and add "decode into fresh wrappers only" to BS6a/BS8's checklist.

**N2. CS0's switch to `pub use dash_pkc::bls::BlsCtx` touches two lines, not one.** `ctx.rs:104` builds `BlsCtx { legacy: … }` and `tx.rs:141` reads `bls.legacy`. They become `BlsCtx::from_legacy(…)` and `bls.is_legacy()`. Correct CS0 §8.3's "one-line follow-up".

**N3. Constructor arguments are inconsistent.** `BlsLazy::new(BlsSchemeKind)` and `from_value(…, BlsSchemeKind)` take a kind, while `from_bytes` takes a `BlsSchemeSource`. Either take a source everywhere or document why construction from a value takes a bare kind.

**N4. Unwiped copies of secret keys.**
- `decode_canonical` returns the secret by value in a tuple (`wrap_codec.rs:298`), which is moved, not wiped.
- `BlsValue::to_bytes` returns a plain `[u8; 32]`.
- Consider `Zeroizing<[u8; N]>` for `SECRET` kinds, or a dedicated `expose_secret`-style accessor. Also note in the docs that every secret `BlsValue` hashes to the same value, so they are poor `HashMap` keys.

**N5. Names deviate from A1's sketch.** A1 has `Scheme`, `SchemeSource`, `PointRepr`; the branch has `BlsSchemeKind`, `BlsSchemeSource`, `BlsPoint`. `BlsPointRepr` already names the encoding type in `group.rs:41`, so `BlsPoint` is a sensible choice. Record it in A1 (M4d).

**N6. `BlsLazy::hash` is easy to confuse with `Hash::hash`.** It is Core's `GetHash`, but a reader will think of the trait. `BlsLazy` deliberately doesn't implement `Hash`. Consider `get_hash` or `serialize_hash`.

**N7. Corpus precision.**
- G1 `tors_3` holds three identical samples (the class is a single point), so mark it `FIXED`.
- The header names the tags, but the probes were linked against the oracle-shim trees (tag + 5 commits). Say so, and add that the shim commits don't touch `src/bls`/`src/dashbls`.

**N8. `BlsSchemeSource::Version(kind)` carries an already-mapped kind,** so the type does not enforce the per-field version mapping. Fine as documentation, but BS6a should centralize the "legacy iff nVersion ∈ {1, 2}" (qc) and "legacy iff nVersion == 1" (ProTx) mappings in one place.

## 9. Conditions on "the legacy rule may replace a relic port", and what remains for BS11a

**Yes.** The rule is exactly equivalent to relic's legacy G1 and G2 deserialization, plus Core's round trip, for all inputs: §2 proves it under the assumptions in §2.7, and §3 confirms it empirically. The implementation computes it correctly. Conditions:

1. **Record the gate evidence.** The manager records §2 (or an equivalent design note) as BS11a's gate-D evidence and applies M4.
2. **BS11a keeps the rest of its scope:**
   - **Exotic semantics beyond decode.** v23.1.8 still accepts a v1 ProRegTx whose legacy operator key is Exotic after V19: `pubKeyOperator.Get().IsValid()` holds (`providertx.cpp:83`), and v24 disallows it only once V24 is active (`specialtxman.cpp:1158-1160`). Core then does relic arithmetic on that key in live paths:
     - `VerifySecureAggregated` over member operator keys (`commitment.cpp:77`);
     - IES DH to the member;
     - `Get() ==` duplicate-key checks. Equality is already exact here.
     
     `diff_bls_verify` with exotic seeds, including the secure-aggregate variant, must show that every such result is unobservable or always false. §2.8 shows the decoder already has relic's exact internal point if a port turns out to be needed.
   - **The IES key** from the basic encoding.
   - **The acceptance run:** O2a's `diff_bls_decode` for ≥ 72 CPU-h.
3. **Pins.** Any re-pin of Core (O17's checklist) re-runs the two-probe corpus generation. A relic or dashbls decode-path change voids §2.

Verdict: **APPROVE-WITH-NITS** (blocker 0, major 0, minor 4, nit 8).
