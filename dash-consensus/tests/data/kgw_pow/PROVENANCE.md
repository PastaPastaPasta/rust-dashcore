# KGW `pow` tables: provenance

Each file holds 4032 lines `m hex(x) hex(pow)`, for m = 1..4032. `x = fl(m / 28.2)` is computed as IEEE binary64, and `pow` is `pow(x, fl(-1.228))`, which is KGW's call (Dash Core `pow.cpp:56`, both pins). That is the call's whole input domain (`CS2-fp-release-binaries.md` §4.1). Values are bit patterns, written in hex.

Every table except `cr.txt` is a measured output of an implementation that a shipped dashd build calls. None is hand-written or derived.

| File | sha256 | Produced by |
|---|---|---|
| `cr.txt` | `cfe0082aaafceded74b09f928aa8b794d28e9d05c53934aea1c9784b38a329fb` | MPFR through gmpy2 2.3.2, 53-bit round-to-nearest. Re-checked at 256 bits. This is the source of `fp::KGW_POW` |
| `glibc-fma.txt` | `02f7a40c9781eab63b0bcbfab1236cb7edc196de990eee5e4a1bed83dd7fcfda` | `tools/kgw_ehd.c` linked against glibc 2.43 x86_64 on a Broadwell host, where the FMA/AVX2 ifunc is selected. Identical outputs: glibc 2.31 and 2.36 (Debian containers, `tools/powdump.py`), glibc 2.43 aarch64 and glibc 2.43 riscv64 (qemu-user with `libc6-{arm64,riscv64}-cross`) |
| `glibc-sse2.txt` | `b3636e6ee1ce30f1b5114b3d1df2f76484826126afde7b678383c3c70da363f4` | The same program with `GLIBC_TUNABLES=glibc.cpu.hwcaps=-AVX2,-FMA` (glibc 2.43), which selects the SSE2 ifunc, as `ld.so --list-diagnostics` confirms. Identical outputs: glibc 2.31 (`-AVX2_Usable,-FMA_Usable`) and 2.36 |
| `mingw-win64-pc64.txt` | `029c852cd10aa00068e71d58c92cabf6476edd3895c5e45ff592c36250e296e0` | The `pow` routine linked into the official `dashd.exe` of v23.1.8 (at `0x140c5d170`) and of v24.0.0-rc.3 (at `0x140d90ff0`). It was executed natively on Linux x86_64 by `tools/winpow.c` under x87 control word `0x37F` (64-bit precision). Both pins give identical outputs, which also equal the CS2 r1 Opus reviewer's independent v24.0.0-rc.3 table |
| `mingw-win64-pc53.txt` | `3a7928ebadbec9b6b7f16680f7583d5e098855c258d0c4fd8fd98b1bd5677074` | The same, under x87 control word `0x27F` (53-bit precision, the Windows x64 default) |

Each table differs from `cr.txt` as follows:
- `glibc-fma`: 6 masses, 1 ulp each;
- `glibc-sse2`: 5 masses, 1 ulp each;
- `mingw-win64-pc64`: 4 masses, 1 ulp each;
- `mingw-win64-pc53`: 3073 masses, up to 5 ulps.

`tools/reproduce.sh` regenerates every measured table and fails if any differs from its file here.

**Copied** (CS2 phase C, design §2.4) from the node program's `designs/CS2-fp-evidence/out/pow-tables/` at commit `4268094b`, unchanged; `tools/` and `reproduce.sh` are there. `cr.txt`'s third column is also what `contrib/node/fp/gen-kgw-pow.py` writes into `src/fp/kgw_pow.rs`, and `fp`'s tests check the two are equal. `tests/quirk_40_kgw_fp_platform_invariance.rs` uses every table here.
