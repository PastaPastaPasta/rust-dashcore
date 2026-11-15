# CS2 floating-point gate: evidence (rev1)

Supports `designs/CS2-fp-release-binaries.md` (rev1) and §2 of `designs/CS2-pow-subsidy.md`.

| Path | What |
|---|---|
| `tools/reproduce.sh WORKDIR` | End-to-end reproduction for all six shipped builds of both pins, from the published archives. It checks GPG signatures against `keys/`, the 152 listing assertions, every measured table against its fixture (asserting which libm variant ran), and the KGW proof. A clean run took 14 min 44 s on agentbox, downloads included |
| `tools/xref.py` | Locates functions in the release binaries: ELF (`.eh_frame`; x86_64, aarch64, riscv64), PE (`.pdata`, IAT) and Mach-O (function starts, symbols, `__stubs`). Resolves data references and prints annotated listings. It regenerates rev0's 12 Linux listings byte-for-byte |
| `tools/check_listings.py` | The 152 machine-checked assertions behind the report's §3, over 36 listings (6 platforms × 2 pins × 3 sites). Also writes `excerpts/` |
| `tools/kgw_ehd.c` | KGW's `pow`/`EventHorizonDeviation` over every reachable `PastBlocksMass`, using the libm it is linked with, separate and fused |
| `tools/powdump.py` | The same `pow` table through `ctypes`, for the older-glibc containers |
| `tools/winpow.c` | Runs the `pow` routine linked into a win64 `dashd.exe` natively, under a chosen x87 control word |
| `tools/kgw_proof.py` | The KGW proof: every measured shipped combination, the two-endpoint ε = 10⁻⁹ check (both roundings), the margins and the tolerance. It is the reference for the design's Rust test 1 |
| `tools/kgw_analyze.py`, `tools/kgw_margins.py` | rev0's Linux-matrix analysis. Kept, and still valid |
| `keys/dash-release-keys.asc` | The public keys that signed `SHA256SUMS.asc`: primaries `29590362…ABE87984`, `15191D05…D01EA524` and `3F5D48C9…400D58D9`, as listed in Dash Core's `contrib/verify-commits/trusted-keys` |
| `excerpts/<pin>-<platform>-{subsidy,kgw,ccbh}.fp.s` | The floating-point lines, with context, of each located function. `reproduce.sh` produces the full listings |
| `out/pow-tables/` | The measured shipped `pow` tables and the correctly rounded table, with `PROVENANCE.md`. These are the phase C fixtures |
| `out/kgw-proof.txt` | `kgw_proof.py` over the fixtures: 0 threshold mismatches; 41 masses with differing `(d, s)`; 16,076 endpoint evaluations pass; 35,751-ulp margin; tolerance 1.534×10⁻⁹ |
| `out/check-listings.txt` | Checker output: 152 PASS, 0 FAIL |
| `out/kgw-analysis.txt`, `out/kgw-margins.txt`, `out/kgw-thresholds.txt`, `out/kgw-pow-cr.txt` | rev0 outputs: the Linux matrix, the thresholds per mass, and the correctly rounded table (the same values as `pow-tables/cr.txt`) |
| `out/pow-variants.md5`, `out/ehd-tables.md5` | rev0 hashes of the per-variant tables |

External sources cited by the report, not copied here:
- glibc 2.31 (`glibc-2.31.tar.xz`, sha256 `9246fe44f68feeec8c666bb87973d590ce0137cca145df014c72ec95be9ffd17`, good signature from `BC7C7372637EC10C57D7AA6579C43DFBF1CF2187`): `sysdeps/ieee754/dbl-64/e_pow.c` (sha256 `de346219ce8f061c…`, lines 27-31) and `sysdeps/x86_64/fpu/multiarch/{e_pow.c, e_pow-fma4.c, ifunc-fma4.h, Makefile}`.
- Gladman, Innocente, Mather, Ozaki and Zimmermann, "Accuracy of Mathematical Functions in Single, Double, Double Extended, and Quadruple Precision" (August 2026), members.loria.fr/PZimmermann/papers/accuracy.pdf (sha256 `9559545b1a6f445234fb969e7909e50be07f42b00e878d15e4fc2b5264a6a402`), binary64 table: Apple 26.5.2 `pow` 0.757 ulp.

Scratch copies (full disassembly, binaries, archives, glibc sources, the paper) live in `/work/scratch/cs2-pow-subsidy/` and are disposable. `reproduce.sh` recreates the parts the report depends on.
