# CS2-fp: floating point in the dashd release binaries (A1 §4.5 rev1, step 1)

| | |
|---|---|
| Status | **rev1, after design review r1** (`reviews/CS2-design-r1-gpt.md`, `reviews/CS2-design-r1-opus.md`, DEC-33). rev0 covered the two Linux builds of each pin; rev1 covers all six shipped builds of each pin. The dispositions are in `CS2-pow-subsidy.md` §12 |
| Date | 2026-10-08 |
| Author | CS2 implementer (Opus 5.5 xhigh), design phase |
| Question | What do the official release binaries actually compute at Core's consensus floating-point sites, and what is the reference the Rust port must equal? (A1 §4.5 rev1: "decide the reference from the binaries, not from glibc") |
| Binaries | All six official builds of each pin from github.com/dashpay/dash releases: `dashcore-{23.1.8,24.0.0-rc.3}-{x86_64,aarch64,riscv64}-linux-gnu.tar.gz`, `-win64.zip`, `-{x86_64,arm64}-apple-darwin.tar.gz`. Checked against each release's `SHA256SUMS.asc`, whose GPG signatures are verified against keys whose primary fingerprints Dash Core lists in `contrib/verify-commits/trusted-keys` (§2) |
| Source read at | Dash Core `v23.1.8` (728f5055) and `v24.0.0-rc.3` (0364ed62) in ~/workspace/dash; glibc 2.31 (signed release tarball) for the `pow` implementations |
| Evidence | `designs/CS2-fp-evidence/`: tools, the floating-point excerpts of all 36 listings, the measured `pow` tables (`out/pow-tables/`), analysis outputs, the pinned release keys. `tools/reproduce.sh WORKDIR` regenerates everything from the published archives |
| Design that uses this | `designs/CS2-pow-subsidy.md` (§2, `fp`) |

The report keeps three kinds of claim apart, because they are proven differently (GPT M3):
- **Operation shape:** what instructions each shipped build executes. Proven by disassembly, for all 12 builds (§3).
- **Measured `pow` results:** the 4032 values each reachable libm returns. Measured wherever the implementation can run on agentbox (§4.1).
- **Error-bound argument:** any `pow` within relative error 10⁻⁹ makes Core's decisions (§4.2). This covers the implementations that cannot run here (glibc's FMA4 variant, Apple's libm) through their documented accuracy.

---

## 0. Findings

1. **The subsidy reference is `x*x`, in every shipped build.** In all 12 builds (six platforms × two pins), each `pow(x, 2.0)` in `GetBlockSubsidyHelper` is a single multiplication (`mulsd x,x`, `fmul d,d,d` or `fmul.d f,f,f`). The function makes no libm call and has no fused multiply-add and no x87 code. Every operation is a correctly rounded IEEE binary64 add, divide or multiply, plus truncating conversions (`cvttsd2si`, `fcvtzs`, `fcvt.l.d … rtz`). A Rust port that performs the same operations in the same order is bit-identical on every input. This is A1 §4.5's first case: the libm port is not needed for the subsidy.
2. **KGW's `pow` is the only libm call in CS2's consensus code** (§3.4). One caller outside CS2's scope has an owner under DEC-29 D4: `llmq/quorumsman.cpp`'s `std::log2` (F1, §7).
3. **KGW's `pow` is a different routine on each platform family:**
   - Linux (x86_64, aarch64, riscv64): the host glibc's `pow`, bound at run time (`pow@GLIBC_2.29`, `@GLIBC_2.27` on riscv64). On x86_64, glibc picks an FMA/AVX2, FMA4 or SSE2 variant by CPU at load time.
   - win64: a **statically linked mingw-w64 `pow`**. `dashd.exe` imports no math function. The routine computes `exp2l(y·log2l(x))` on the x87 unit, so its result depends on the x87 precision-control field of the validating thread.
   - macOS (x86_64, arm64): Apple's system libm `_pow`, through `__stubs`.
4. **The horizon `1 + 0.7084·pow(…)` is rounded two ways:**
   - separate `mulsd` + `addsd` (two roundings): x86_64 Linux, win64, x86_64 macOS;
   - one fused multiply-add (one rounding): aarch64 Linux (`fmadd`), riscv64 Linux (`fmadd.d`), arm64 macOS (`fmadd`).

   All six builds of both pins follow this split. Guix GCC 13.3 (Linux, win64) and clang 19 (macOS) both contract `a*b + c` by default where the ISA baseline has an FMA; baseline x86-64 has none.
5. **At least four distinct `pow` tables ship** over KGW's 4032-point domain (§4.1):
   - **A**: glibc FMA ifunc (2.31, 2.36, 2.43) = glibc aarch64 = glibc riscv64;
   - **B**: glibc SSE2 ifunc;
   - **W64**: win64 at x87 64-bit precision;
   - **W53**: win64 at 53-bit precision.

   A, B and W64 are within 1 ulp of correctly rounded everywhere (A off at 6 masses, B at 5, W64 at 4). W53 is off at 3073 masses, by up to 5 ulps. glibc's FMA4 variant and Apple's libm cannot run here; their published accuracy is in §4.3.
6. **Core has no bit-exact KGW reference, but it has an exact KGW *decision* reference.** Across the six measured shipped combinations (A×separate on x86_64 FMA hosts, B×separate on other x86_64 hosts, A×fused on aarch64 and riscv64, W64×separate and W53×separate on win64), `(EventHorizonDeviation, 1/EventHorizonDeviation)` differs at **41** masses. Yet every combination breaks out of the KGW loop at the same point, for every mass and every possible block-time gap (§4.2). The closest any input comes to a boundary is **35,751 ulps**.

   More generally, the decisions are identical for **any** `pow` whose 4032 results are within relative error 10⁻⁹ of the true value, under either rounding. A two-endpoint check proves this. The measured tolerance is 1.534×10⁻⁹, about 6.9 million ulps. KGW's output is one deterministic function on its whole input domain, for every shipped build.
7. **The mainnet ≤ 68589 tolerance check is the same expression on every build.** `abs(n1-n2) > n1*0.5` uses the double `fabs` (`andpd` with the sign mask, `fabd`, or `fabs.d`), a multiply by 0.5 and an ordered compare, with nothing contracted.
8. **The O1 oracle shim reproduces the x86_64 Linux release shape.** Its archives (host GCC 15.2, x86_64) fold the subsidy squares, call `pow` once in KGW followed by `mulsd`/`addsd`, and use `andpd` in the tolerance check. It is a valid differential reference for these sites.

**What this changes** (design §2): no `fp::pow` port. The subsidy is plain IEEE arithmetic in `fp`. KGW uses a 4032-entry table of correctly rounded `pow` values. The proof is platform-independent (the two-endpoint ε check); the measured shipped tables are checked in as fixtures; the oracle checks both sides of every decision boundary.

---

## 1. Method

**Integrity.**
- Each release's `SHA256SUMS.asc` is a clear-signed file. Its signatures were verified with GPG:
  - v24.0.0-rc.3: one good signature, from key `29590362EC878A81FD3C202B52527BEDABE87984`;
  - v23.1.8: three good signatures, from `29590362EC878A81FD3C202B52527BEDABE87984`, `15191D05B5CF956FE37C95962176C4A5D01EA524` and `3F5D48C9F00293CD365A3A9883592BD1400D58D9`.

  Those are exactly the primary keys in Dash Core's `contrib/verify-commits/trusted-keys` at both pins. The keys were fetched from public keyservers; `evidence/keys/dash-release-keys.asc` pins them.
- `reproduce.sh` fails unless every signature is good and from a listed key, and every archive matches the signed checksums.
- glibc 2.31 was taken from `glibc-2.31.tar.xz` (sha256 `9246fe44…ffd17`), with a good signature from the release manager's key `BC7C7372637EC10C57D7AA6579C43DFBF1CF2187`.

**Locating code.** The Linux and win64 binaries are stripped, and Dash publishes no debug archives. The macOS binaries keep their symbols.

| Format | Function bounds | Data references | Imported calls |
|---|---|---|---|
| ELF (x86_64, aarch64, riscv64) | `.eh_frame` FDEs | RIP-relative (x86_64), `adrp`+`add`/`ldr` (aarch64), `auipc`+`addi`/`fld`/`ld` (riscv64) | PLT stubs through `.rela.plt` |
| PE32+ (win64) | `.pdata` (`RUNTIME_FUNCTION`) | RIP-relative | IAT slots through the import directory |
| Mach-O (x86_64, arm64) | `LC_FUNCTION_STARTS`, plus symbol names | as for ELF on the same ISA | `__stubs` through the indirect symbol table |

**Anchors:**
- `GetBlockSubsidyHelper`: the `assert(isDevnet)` strings. Exactly one function per binary references them, so the helper is emitted once and not inlined elsewhere. On macOS, the symbol `__ZL21GetBlockSubsidyHelperiiRKN9Consensus6ParamsEb` is used.
- KGW: the constants −1.228 and 0.7084, from `.rodata`/`.rdata`/`__literals`, or materialised by `mov`/`movk` on arm64 macOS. One function per binary, which also carries `GetNextWorkRequired`'s and `DarkGravityWave`'s assert strings. On macOS it is the symbol `__Z19GetNextWorkRequired…`.
- Tolerance check: the log string `"ERROR: %s : incorrect proof of work (DGW pre-fork) …"`. x86_64 Linux splits that function into hot and `.cold` parts, and the FP code is in the hot part. On macOS it is `__ZL26ContextualCheckBlockHeader…`.
- win64's `pow`: the last call before KGW's `mulsd …, 0.7084`. `check_listings.py` finds it independently: `0x140c5d170` (v23.1.8) and `0x140d90ff0` (v24.0.0-rc.3).

**Tooling** (`evidence/tools`):
- `xref.py`: capstone 5.0.7, pyelftools and LIEF 1.0. It also reproduces rev0's 12 Linux listings byte-for-byte.
- `check_listings.py`: 152 assertions over the 36 listings, all passing.
- `kgw_ehd.c`, `powdump.py` and `winpow.c` (the last runs the win64 routine natively; adapted from the Opus reviewer's tool).
- `kgw_proof.py` (MPFR through gmpy2 2.3.2). rev0's `kgw_analyze.py` and `kgw_margins.py` are kept for the Linux matrix.

---

## 2. Binaries

| Build | Archive sha256 (matches the signed `SHA256SUMS`) | dashd id |
|---|---|---|
| 23.1.8 x86_64 Linux | `e664cad0fe860afa3d005be0db1eb06da598f2f83421083afae77191e3115825` | BuildID `cd2a3378c52fadc0ddac57e5ddeadf40e825d781` |
| 23.1.8 aarch64 Linux | `b96fef477ea01f5c47cc3d3f7de3fdd0bc8cc86f64660f720e8206d889604523` | BuildID `e68501496883b4fd85e0617401d2b1f2cf71b05c` |
| 23.1.8 riscv64 Linux | `dbdf03365cb83bbcc189f68db5e1cbc89fcaafff9c45a142a3f3a907f7f21947` | BuildID `73d963754273e45b2ada92a55b5898ebfbd97368` |
| 23.1.8 win64 | `340cf8ec35a84eaf72d113f04d21c4d1b02272425daab0df98b57d5db62a782d` | `dashd.exe` sha256 `069f8434…ff3e385` |
| 23.1.8 x86_64 macOS | `e95337f0a12deae9b949ca791ba5704ad25c28f13d361ab9cdab4784226cd835` | `dashd` sha256 `e84a5a10…8dcfd95` |
| 23.1.8 arm64 macOS | `a3db11790722d3ca08a5205aa985cb7a8a12f649e6bed7c46d6de480b25c14b5` | `dashd` sha256 `9419ae31…9a432e2` |
| 24.0.0-rc.3 x86_64 Linux | `9327f00018afd12f11676ac66e688c76b253e4783bf0d595f08476de4269ff52` | BuildID `d6ce8c1650f4b999b724a1fbebe8d285a846832b` |
| 24.0.0-rc.3 aarch64 Linux | `bf492bef6069949088ec338aa57d654c82525ffff8c7a2014f5b970cb40f2f94` | BuildID `e05a27c2f4b13a591b55a8314c604f10c6c328e6` |
| 24.0.0-rc.3 riscv64 Linux | `42c7210acf92324bca8a1dc35a73a60f8a5a2d32fd21edaae829edf14c48d143` | BuildID `d831113fa5849f9a2c0a96aa19b90548244c9ede` |
| 24.0.0-rc.3 win64 | `bd92e1d5682f8a2f3d4cd17d7aa52391317d1053146a098393242003de8aea05` | `dashd.exe` sha256 `fccb8f47…c3f7eaf` |
| 24.0.0-rc.3 x86_64 macOS | `b626c2f2db79619e21bca8bf7b23b2eb326b9e53d12fcae525c6df8535898371` | `dashd` sha256 `03e5653e…c87177c` |
| 24.0.0-rc.3 arm64 macOS | `b1a63de5dd98fb1ef32e091cb02d8891c005aafb452b0a717746f1ed28ddf7ef` | `dashd` sha256 `78d9a145…3c87f77` |

The x86_64 Linux tarballs are byte-identical to the ones B1 and BS2 use: v23.1.8 in `/work/bench/dl`, and v24.0.0-rc.3 in both `/work/bench/dl` and `/work/chains/bin/dl`.

**Toolchains** (`contrib/guix/manifest.scm`, `contrib/guix/libexec/build.sh`, `depends/hosts/*.mk`; the floating-point-relevant settings are identical at both pins):

| Platform | Compiler | Flags | libm |
|---|---|---|---|
| Linux x86_64, aarch64, riscv64 | Guix GCC 13.3.0, glibc 2.31 sysroot | `-O2`, no `-march`, no `-ffp-contract` | host glibc at run time |
| win64 | Guix mingw-w64 GCC 13.3.0 (`GCC: (GNU) 13.3.0` in the binary) | `-O2` | mingw-w64 runtime, linked statically |
| macOS x86_64, arm64 | Guix `clang-toolchain-19`, Xcode 14.0 SDK | depends' defaults | Apple libm at run time |

**How `pow` is reached:**
- Linux: `readelf --dyn-syms` shows `pow@GLIBC_2.29` on x86_64 and aarch64, and `pow@GLIBC_2.27` on riscv64.
- win64: the import table (eight DLLs, `msvcrt.dll` among them) contains no math function.
- macOS: `_pow` through `__stubs`.

**Function locations:**

| Build | `GetBlockSubsidyHelper` | `GetNextWorkRequired` (KGW, DGW inlined) | `ContextualCheckBlockHeader` |
|---|---|---|---|
| 23.1.8 x86_64 Linux | 0x64abf0–0x64b145 | 0x45fc40–0x46078f | 0x653950–0x6545bd (+ .cold 0x12ad58) |
| 23.1.8 aarch64 Linux | 0x4dc010–0x4dc500 | 0x2f0270–0x2f0c30 | 0x4e6650–0x4e7334 |
| 23.1.8 riscv64 Linux | 0x490fe4–0x49141a | 0x2ad310–0x2adde8 | 0x49ae92–0x49ba78 |
| 23.1.8 win64 | 0x14038dab0–0x14038dfcb | 0x140196210–0x140196e64 (`pow` at 0x140c5d170) | 0x14039a440–0x14039b358 |
| 23.1.8 x86_64 macOS | 0x10074a450–0x10074aa50 | 0x100490ee0–0x100491bf0 | 0x100765ba0–0x100766ea0 |
| 23.1.8 arm64 macOS | 0x10065b7d8–0x10065bd2c | 0x10041783c–0x100418274 | 0x100672d44–0x100673c00 |
| 24.0.0-rc.3 x86_64 Linux | 0x719e10–0x71a387 | 0x4ec8b0–0x4ed3ff | 0x725620–0x7264fb (+ .cold 0x1472e3) |
| 24.0.0-rc.3 aarch64 Linux | 0x587db0–0x5882e0 | 0x35c7a0–0x35d160 | 0x594ee0–0x595e18 |
| 24.0.0-rc.3 riscv64 Linux | 0x529d20–0x52a180 | 0x304220–0x304cf8 | 0x5363c6–0x5371d4 |
| 24.0.0-rc.3 win64 | 0x14041cb50–0x14041d07d | 0x1401dd730–0x1401de384 (`pow` at 0x140d90ff0) | 0x140429f10–0x14042afc9 |
| 24.0.0-rc.3 x86_64 macOS | 0x10081b0c0–0x10081b580 | 0x1004ef250–0x1004eff60 | 0x100836f40–0x1008380e0 |
| 24.0.0-rc.3 arm64 macOS | 0x10072b334–0x10072b768 | 0x100470cb4–0x1004716f0 | 0x100742c88–0x100743a40 |

---

## 3. The sites

### 3.1 `GetBlockSubsidyHelper` (`validation.cpp`, v24.0.0-rc.3:1809, v23.1.8:1483)

Source (identical at both pins apart from line numbers):
```cpp
dDiff = ConvertBitsToDouble(nPrevBits);      // or 65535.0/(nPrevBits & 0xffffff) on mainnet, nPrevHeight <= 4500
...
nSubsidyBase = (1111.0 / (pow((dDiff+1.0),2.0)));          // nPrevHeight < 5465, clamp [1, 500]
nSubsidyBase = (11111.0 / (pow((dDiff+51.0)/6.0,2.0)));    // < 17000, or dDiff <= 75 and < 24000; clamp [25, 500]
nSubsidyBase = (2222222.0 / (pow((dDiff+2600.0)/9.0,2.0)));// otherwise, clamp [5, 25]
```

v24.0.0-rc.3 x86_64 Linux, formula 3 (the other formulas, and win64 and x86_64 macOS, have the same shape):
```
71a15c:  cmp      r13d, 0x5dbf                     ; nPrevHeight vs 23999
71a165:  movsd    xmm1, 75.0
71a16d:  comisd   xmm1, xmm0                       ; dDiff <= 75 -> formula 2 (jae)
71a173:  addsd    xmm0, 2600.0
71a17b:  divsd    xmm0, 9.0
71a183:  mulsd    xmm0, xmm0                       ; pow(x, 2.0) == x*x
71a187:  movsd    xmm1, 2222222.0
71a194:  divsd    xmm1, xmm0
71a198:  cvttsd2si rcx, xmm1                       ; truncation toward zero, int64
71a19d:  cmp/cmovl 5 ; cmp/cmovg 25                 ; clamps, after truncation
71a1b0:  imul     rcx, rcx, 0x5f5e100              ; * COIN
```
v24.0.0-rc.3 aarch64 Linux, formula 3. Formula 2 loads 51.0, 6.0 and 11111.0, then branches into this same tail at `5880f8`:
```
5880e8:  fadd     d1, d8, d0                       ; + 2600.0
5880f8:  fdiv     d1, d1, d2                       ; / 9.0
588104:  fmul     d1, d1, d1                       ; pow(x, 2.0) == x*x
588108:  fdiv     d0, d0, d1                       ; 2222222.0 / x²
58810c:  fcvtzs   x0, d0
```
v24.0.0-rc.3 riscv64 Linux, formula 3:
```
52a040:  fdiv.d   fa5, fa5, fa3                    ; / 9.0
52a044:  fmul.d   fa5, fa5, fa5                    ; pow(x, 2.0) == x*x
52a048:  fdiv.d   fa5, fa4, fa5                    ; 2222222.0 / x²
52a04c:  fcvt.l.d a0, fa5, rtz                     ; truncation toward zero
```

| Build | Squares as `x*x` | Truncations | `pow` calls | FMA | x87 |
|---|---|---|---|---|---|
| x86_64 Linux, win64, x86_64 macOS | 3 `mulsd` | 3 `cvttsd2si` | 0 | 0 | 0 |
| aarch64 Linux | 2 `fmul` (formulas 2 and 3 share a tail) | 2 `fcvtzs` | 0 | 0 | — |
| arm64 macOS | 3 `fmul` | 3 `fcvtzs` | 0 | 0 | — |
| riscv64 Linux | 3 `fmul.d` | 3 `fcvt.l.d … rtz` | 0 | 0 | — |

The table holds for both pins (`check_listings.py`).

`ConvertBitsToDouble` is inlined:
- `65535.0 / (double)(nBits & 0xffffff)`. The mask keeps all 24 low bits, sign bit included. `SetCompact` masks 23 bits, so the two must not be conflated;
- then a loop of `*= 256.0`;
- or a loop of `*= 0.00390625` for the `/= 256.0` direction. Multiplying by 2⁻⁸ is the same correctly rounded operation as dividing by 2⁸.

Each step is a separate rounding, so the loop must not become one `ldexp`. That matters only for subnormal results: the first subnormal value appears at size byte 156 (0x9c), far outside every valid `nBits`. The port keeps the loop anyway.

The mainnet `nPrevHeight <= 4500` path is `and r15d, 0xffffff; cvtsi2sd; divsd` with no scaling loop (x86_64 v24 at `71a2df`).

### 3.2 KGW (`pow.cpp:15-79`, inlined into `GetNextWorkRequired`)

```cpp
PastRateAdjustmentRatio = double(PastRateTargetSeconds) / double(PastRateActualSeconds);
EventHorizonDeviation = 1 + (0.7084 * pow((double(PastBlocksMass)/double(28.2)), -1.228));
EventHorizonDeviationFast = EventHorizonDeviation;
EventHorizonDeviationSlow = 1 / EventHorizonDeviation;
if (PastBlocksMass >= PastBlocksMin)
    if ((PastRateAdjustmentRatio <= EventHorizonDeviationSlow) || (PastRateAdjustmentRatio >= EventHorizonDeviationFast)) break;
```

v24.0.0-rc.3 **x86_64 Linux** (separate):
```
4ecbbc:  divsd    xmm2, xmm0                       ; ratio
4ecbe2:  divsd    xmm0, 28.2
4ecbea:  call     pow@plt
4ecbfb:  mulsd    xmm0, 0.7084                     ; rounding 1
4ecc13:  addsd    xmm0, xmm1(1.0)                  ; rounding 2  -> EHD
4ecc17:  divsd    xmm1, xmm0                       ; 1/EHD
4ecc1b:  comisd   xmm1, xmm2                       ; ratio <= slow
4ecc25:  comisd   xmm2, xmm0                       ; ratio >= fast
```
v24.0.0-rc.3 **aarch64 Linux** (fused):
```
35ca58:  fdiv     d8, d8, d0                       ; ratio
35ca64:  fdiv     d0, d0, d9                       ; / 28.2 (0x403c333333333333)
35ca68:  bl       pow@plt
35ca70:  cmp      x0, x25 ; b.hi                   ; PastBlocksMass >= PastBlocksMin (EHD computed only when used)
35ca7c:  fmadd    d0, d0, d11, d1                  ; fma(pow, 0.7084, 1.0): ONE rounding -> EHD
35ca80:  fdiv     d1, d1, d0                       ; 1/EHD
35ca84:  fcmpe    d1, d8 ; b.ge break              ; ratio <= slow
35ca8c:  fcmpe    d0, d8 ; b.ls break              ; ratio >= fast
```
v24.0.0-rc.3 **riscv64 Linux** (fused): `fdiv.d fa0, fa0, fs2` (28.2), `jalr … <pow@plt>`, `fmadd.d fa5, fa0, fs4(0.7084), fs1(1.0)`, `fdiv.d fa4, fs1, fa5`.

v24.0.0-rc.3 **win64** (separate):
```
1401dda7e:  divsd    xmm0, xmm6                    ; / 28.2
1401dda82:  call     0x140d90ff0                   ; mingw-w64 pow, linked into dashd.exe
1401dda90:  mulsd    xmm0, 0.7084                  ; rounding 1
1401ddaa0:  addsd    xmm0, xmm1(1.0)               ; rounding 2
1401ddaa4:  divsd    xmm1, xmm0                    ; 1/EHD
```
v24.0.0-rc.3 **x86_64 macOS** (separate): `divsd 28.2`, `call _pow` (stub), `mulsd 0.7084`, …, `addsd xmm0, xmm2(1.0)`, `divsd`, then two `ucomisd`.

v24.0.0-rc.3 **arm64 macOS** (fused): `fdiv d0, d0, d1(28.2)`, `bl _pow` (stub), then 0.7084 materialised by `mov/movk #0x3fe6ab367a0f9097`, `fmadd d0, d0, d1, d8(1.0)`, `fdiv d1, d8, d0`.

v23.1.8 has the same shapes on every platform. Its addresses are in §2 and the excerpts are under `evidence/excerpts/v23-*-kgw.fp.s`.

**The win64 `pow`** (`0x140c5d170` in v23.1.8, `0x140d90ff0` in v24.0.0-rc.3; the same 315-instruction routine) is mingw-w64's. For normal arguments it does:
- `fld x` into the x87 stack and spill it as an 80-bit temporary;
- call an internal `log2l`, then `fmul y` in 80-bit registers;
- call an internal `exp2l`, then `fstp` to a double.

Special cases go to `_errno` (its only import). The `fmul` and the internal routines are rounded according to the x87 precision-control field of the thread that runs them. mingw's startup `fninit` leaves 64-bit precision (`0x37F`) in the main thread; the Windows x64 default control word is 53-bit (`0x27F`). Which one dashd's validation thread uses was not established, and both were measured (§4.1).

The rest of KGW's floating point is `pastSecondsMin = nPowTargetTimespan * 0.025`. That is `86400.0 * 0.025`, which rounds to exactly 2160.0 and converts to the integer 2160 on every build.

### 3.3 The mainnet ≤ 68589 tolerance (`validation.cpp:4474-4483` v24, `:4108-4117` v23)

```cpp
double n1 = ConvertBitsToDouble(block.nBits);
double n2 = ConvertBitsToDouble(nBitsNext);
if (abs(n1-n2) > n1*0.5) → "bad-diffbits"
```
- x86_64 Linux, win64 and x86_64 macOS: `mulsd 0.5; subsd; andpd 0x7fffffffffffffff; comisd`.
- aarch64 Linux and arm64 macOS: `fabd; fmul 0.5; fcmpe`.
- riscv64: `fsub.d; fabs.d; fmul.d 0.5; flt.d`.

All are `fabs(round(n1 − n2)) > n1 · 0.5`: `abs` is the double overload, and nothing is contracted. The comparison is false on NaN, so the rule rejects only when the difference is a number greater than `n1·0.5` (design §2.3, §2.5 F3).

### 3.4 The other libm call sites

The other `pow` call sites are not consensus:

| Function | Anchor | Use |
|---|---|---|
| `CTxMemPool::GetMinFee` | 21600.0, 43200.0 (the half-lives), `pow(2.0, …)` | rolling minimum fee (policy) |
| `AddrInfo::GetChance` | 0.66 | address selection |
| `AddrManImpl::Select_` | 0.66, `"addrman.cpp"` | address selection |

The Linux builds have all four call sites. macOS has two (`AddrManImpl::Select_`, with `GetChance` inlined, and `GetNextWorkRequired`), because clang turns `GetMinFee`'s `pow(2.0, t)` into `exp2(t)`.

**The CS2 functions call no libm routine except KGW's `pow`:**
- Each located `GetBlockSubsidyHelper` and `ContextualCheckBlockHeader`, on every build, calls only `assert`, stack-protector, `mem*` and mutex imports, plus internal functions (`Params()`, string compares, logging).
- `GetNextWorkRequired` adds exactly one `pow` call.

At the source level, a `git grep` for every C/C++ math function (`pow`, `log*`, `exp*`, `ceil`, `floor`, `round`, `l(l)round`, `sqrt`, `cbrt`, `fmod`, `trunc`, `ldexp`, `frexp`, trigonometric functions) over both pins covers `src/{consensus,script,primitives,evo,llmq,governance,masternode,chainlock,instantsend,coinjoin,kernel}/`, plus `versionbits.cpp`, `deploymentstatus.cpp`, `chain.cpp`, `arith_uint256.cpp`, `pow.cpp`, `validation.cpp` and `node/miner.cpp`. It finds exactly:
- the three subsidy `pow(x, 2.0)` calls and KGW's `pow`;
- the `log2_work` log lines in `validation.cpp`;
- `llmq/quorumsman.cpp` `std::log2` (twice in v24, once in v23; F1);
- `llmq/signing_shares.cpp` `exp2` (a resend timer).

In the x86_64 Linux binary, the callers of the other libm imports were identified by string anchors (v24.0.0-rc.3). Not every caller has an anchor, so the table names what was identified:

| Import | Identified callers | Consensus? |
|---|---|---|
| `log` | `UpdateTipLog`, `InvalidChainFound`, `ConflictingChainFound` (`log2_work` in log lines, `validation.cpp:2049-3181`); three unanchored functions, one reached from net-processing code | not in any consensus source file (`git grep`) |
| `log2` | `SelectCommitmentForSigning` and a sibling (`llmq/quorumsman.cpp:739,811`) | **LLMQ, owned by LQ1 (DEC-29 D4)**; F1, §7 |
| `exp2` | `CollectSigSharesToSendConcentrated` (`llmq/signing_shares.cpp:1051`, a resend wait time) | no (timing; L6 fingerprint only) |
| `llround` | `CTxMemPool::GetMinFee`, `policy/fees.cpp` | no (policy) |
| `ceil`, `round`, `ldexp`, `frexp`, `exp` | init (`" block index %15dms"`), `policy/fees.cpp`, RPC fee-rate fields; others unanchored | not in any consensus source file (`git grep`) |
| `fmod`, `trunc`, `sqrt` | no direct caller in dashd's code | — |

---

## 4. KGW's libm domain

### 4.1 The domain is 4032 points; the measured tables

`pow`'s arguments are `x = fl(m / 28.2)` and `y = fl(−1.228)`, where `m = PastBlocksMass` is an integer in `1..=PastBlocksMax`.

KGW is reachable **only on mainnet**:
- testnet and devnet set `nPowKGWHeight = nPowDGWHeight`, and no override changes either;
- regtest returns powLimit before KGW (`fPowNoRetargeting`, `pow.cpp:187`).

On mainnet `PastBlocksMax = 604800/150 = 4032`, and the result is used only when `m ≥ PastBlocksMin = 14`. Header `nBits` and timestamps never reach `pow`.

`pow` was evaluated on all 4032 points under every implementation dashd can bind to that runs on agentbox, and compared with MPFR's correctly rounded value. The tables are fixtures in `evidence/out/pow-tables/`, as `m hex(x) hex(pow)`:

| Fixture | Implementation and how it was run | Differs from correctly rounded |
|---|---|---|
| `cr.txt` | MPFR 53-bit round-to-nearest (gmpy2); the design's `KGW_POW` | — |
| `glibc-fma.txt` (**A**) | glibc x86_64 FMA ifunc: 2.43 on the host, 2.31 and 2.36 in Debian containers (`ctypes`). Equal to glibc 2.43 aarch64 and riscv64 under qemu-user | 6 masses {1695, 2109, 2337, 2381, 3083, 3111}, 1 ulp each |
| `glibc-sse2.txt` (**B**) | glibc x86_64 SSE2 ifunc, the same versions. Forced with `GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA` (2.33+) or `-AVX2_Usable,-FMA_Usable` (2.31); the run checks that the variant actually changed (`ld.so --list-diagnostics`, and the output differs from A) | 5 masses {851, 2109, 2381, 3083, 3111}, 1 ulp each |
| `mingw-win64-pc64.txt` (**W64**) | the routine inside each shipped `dashd.exe`, run natively by `winpow.c` with x87 control word `0x37F`. v23.1.8 and v24.0.0-rc.3 give identical tables | 4 masses {194, 851, 2381, 3111}, 1 ulp each |
| `mingw-win64-pc53.txt` (**W53**) | the same, with control word `0x27F` | 3073 masses, up to 5 ulps |

These match the Opus reviewer's independent v24.0.0-rc.3 win64 tables bit-for-bit.

The horizon `d` differs between the six measured shipped combinations at **41** masses (14..4032):

{14, 15, 17, 20, 21, 29, 30, 37, 38, 39, 49, 61, 78, 83, 86, 90, 95, 97, 98, 99, 124, 156, 200, 201, 268, 342, 364, 454, 591, 956, 1066, 1458, 1548, 1601, 1807, 1850, 1996, 2479, 2855, 3007, 3062}

The combinations are A×separate, B×separate, A×fused (aarch64, riscv64), W64×separate and W53×separate. Tables A and B alone give the same `d` everywhere, because the 1-ulp `p` differences vanish in `1 + 0.7084·p`. The fused rounding and the win64 tables make the differences.

### 4.2 The decisions are identical everywhere

The break test is `ratio ≤ s ∨ ratio ≥ d`, with `ratio = fl(150m / a)`, and `a = PastRateActualSeconds`.

`a` is an `int64` clamped to ≥ 0. It is the difference of two `uint32` `nTime` values, so `a ∈ [0, 2³²−1]`. When `a = 0` the ratio is the literal 1.0, and `s < 1 < d` for every `m`. `fl(150m/a)` is non-increasing in `a`, so for each `m` the break set is exactly `a ≤ A_fast(m) ∨ a ≥ A_slow(m)`. These are two integers, found by bisection with IEEE division.

`kgw_proof.py` checks three things (`evidence/out/kgw-proof.txt`):
1. **Measured combinations.** All six have the implementation's thresholds (the correctly rounded table with separate rounding) at every mass. There are 0 mismatches.
2. **Any `pow` within 10⁻⁹.**
   - The horizon is non-decreasing in `p` under both roundings, `s = fl(1/d)` is non-increasing, and `A_fast`/`A_slow` are monotone in `d`/`s`.
   - So if the thresholds at `p_lo` and `p_hi` equal the implementation's, they are equal for every `p` in between.
   - With `p_lo = fl(p_cr·(1−10⁻⁹))` and `p_hi = fl(p_cr·(1+10⁻⁹))`, each widened outward by 4 ulps, the interval contains every double within relative error 10⁻⁹ of the exact `pow`.
   - All 16,076 endpoint evaluations (4019 masses × 2 endpoints × 2 roundings) pass.

   Hence **any `pow` whose 4032 results are within 10⁻⁹ of the true value reproduces Core's decisions, with either rounding.**
3. **Margins.**
   - The smallest distance between any `fl(150m/a)` and any combination's `d` or `s` is **35,751 ulps** of the boundary, at m = 2712, a = 407858. The thresholds there are 405744 and 407859.
   - The largest relative `pow` error that keeps every decision is **1.534×10⁻⁹**, also at m = 2712. That is about 6.9 million ulps of `p`.

The Opus reviewer re-derived the thresholds in exact rational arithmetic and found 0 mismatches. The GPT reviewer checked the 1.5×10⁻⁹ interval with outward widening independently.

### 4.3 The implementations that cannot run here

| Implementation | Where it runs | Accuracy | Covered by §4.2 item 2 because |
|---|---|---|---|
| glibc x86_64 **FMA4** ifunc | Linux x86_64 dashd on CPUs with FMA4 but not FMA+AVX2 (older AMD). glibc 2.31 `sysdeps/x86_64/fpu/multiarch/ifunc-fma4.h` selects it only then | `e_pow-fma4.c` compiles the same `sysdeps/ieee754/dbl-64/e_pow.c` with `-mfma4` (`multiarch/Makefile:60`). That file documents "Worst-case error: 0.54 ULP" (`e_pow.c:29`, glibc 2.31; sha256 `de346219…6e339e`) | 0.54 ulp ≪ 10⁻⁹ relative |
| **Apple libm** `_pow` | macOS x86_64 and arm64 dashd | Apple publishes no bound. Gladman, Innocente, Mather, Ozaki and Zimmermann, "Accuracy of Mathematical Functions…" (August 2026), binary64 table: largest known `pow` error **0.757 ulp** for Apple Math Library 26.5.2 (Darwin 25.5.0, measured on an M1). No x86_64 macOS measurement is published | arm64: 0.757 ulp ≪ 10⁻⁹. x86_64 macOS: **assumption**, stated in the design, that Apple's x86_64 `pow` is within 10⁻⁹ relative error at these 4032 points. 10⁻⁹ is about 4.5 million ulps; the worst `pow` in the same table (636 ulps, OpenLibm and FreeBSD) is four orders of magnitude inside it |

No Mac run is needed (DEC-33 b). A run of `kgw_ehd.c` on a Mac would turn the x86_64 macOS assumption into a measurement; it is optional (§7, F3).

---

## 5. What this means for A1 §4.5

| A1 §4.5 rev1 | Outcome |
|---|---|
| Step 1: "if the release binaries compute `x*x`, the subsidy reference **is** `x*x` … v1's port of the glibc `pow` routine is not needed for the subsidy" | Confirmed for all 12 builds (§3.1) |
| "A libm port is kept only for call sites the binaries actually route to libm: KGW's `pow(x, −1.228)`" | Not needed. The domain is 4032 points (§4.1). Core itself ships at least four bitwise-different `pow` tables and two roundings, whose decisions provably agree. The proof covers any `pow` within 10⁻⁹ (§4.2). The design uses a correctly rounded table (design §2.4) |
| "For any call site that keeps a libm port: 10⁸ random inputs against glibc on x86_64 (FMA and non-FMA ifuncs) and aarch64, and against the release binary's own routine" | Replaced by an exhaustive check: all 4032 `pow` inputs × every `a ∈ [0, 2³²−1]` (through the thresholds). It runs against every measured shipped routine, win64's own routine included, plus the platform-independent ε interval. This is strictly stronger and needs no sampling |
| "`f64` banned outside `fp`; libm calls banned everywhere outside `fp`" | Holds, and the design goes further: **no libm call anywhere in `dash-consensus`, `fp` included** (design §2.6) |
| Admissible domain gate | Design §2.5: KGW is closed by §4.2. The subsidy's domain is every compact a valid header can carry plus `nBits = 1`, closed numerically, with the all-`u32` sweep as an explicit gate. The tolerance check is IEEE basic operations only |

**Errata (DEC-29 D5).** A1 §2.4's "KGW (`f64` and `fp::pow`)" should read "KGW (`f64`; `pow` replaced by the correctly rounded table `fp::KGW_POW`)". R4-B §3.3's "Use `f64::powf`. Risk: low" is superseded: `powf` would call the target's libm. The risk assessment stands, now with a proof.

---

## 6. Reproduce

```
bash designs/CS2-fp-evidence/tools/reproduce.sh /work/scratch/<slug>/fp
```

The script:
1. downloads all six archives of both pins;
2. verifies the GPG signatures against the pinned keys and the checksums;
3. indexes the twelve `dashd` binaries, dumps the 36 site listings and runs `check_listings.py` (152 assertions);
4. measures the `pow` tables: host glibc under both x86_64 ifuncs, with the variant asserted; aarch64 and riscv64 under qemu-user; optionally glibc 2.31 and 2.36 in containers; and win64's routine under both control words. Each table is compared with its fixture;
5. runs `kgw_proof.py`.

It needs `gh`, `gpg`, `gcc`, `gcc-{aarch64,riscv64}-linux-gnu`, `qemu-user`, `libc6-{arm64,riscv64}-cross`, `binutils-mingw-w64-x86-64`, `unzip` and `python3`; docker is optional.

A clean run on agentbox took 14 min 44 s, including downloading the 12 archives (about 700 MB) and the two glibc containers. It reproduced all 36 listings byte-for-byte (rev0's 12 Linux listings among them), passed all 152 checks and all 12 variant assertions, matched every fixture, and reproduced `kgw-proof.txt`.

---

## 7. Follow-ups (not CS2)

- **F1 (LQ1, DEC-29 D4).** `llmq/quorumsman.cpp:739,811`: `int n = std::log2(llmq_params.signingActiveQuorumCount)`.
  - The call is only reached when `IsQuorumRotationEnabled`. The rotation types' counts are 2 (`LLMQ_TEST_DIP0024`, `LLMQ_DEVNET_DIP0024`) and 32 (`LLMQ_60_75`), both powers of two. `-llmqdevnetparams` overrides only size and threshold.
  - On those inputs every `log2` returns the exact integer, and the Rust port can use `trailing_zeros`.
  - The libm is per platform here too: glibc on Linux, a static mingw-w64 x87 `log2` on win64 (precision-control dependent, like its `pow`), and Apple's on macOS. So LQ1's domain check should state that the integer result holds for every shipped libm, which it does for exact powers of two. LQ1 must still check that no count outside {2, 32} can reach the call.
- **F2 (O17 PIN-DELTA).** At every re-pin, re-run `reproduce.sh` on **all six** builds of the new tag. Re-check the subsidy shape, the KGW horizon rounding per platform, which `pow` each build reaches, and the tolerance shape. Add any new shipped `pow` table as a fixture.
- **F3 (optional, pasta).** Running `tools/kgw_ehd.c` once on a Mac (arm64 natively, and x86_64 under Rosetta) would replace the x86_64 macOS assumption of §4.3 with a measurement. DEC-33 does not require it.
