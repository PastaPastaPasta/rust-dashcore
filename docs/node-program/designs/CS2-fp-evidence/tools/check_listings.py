#!/usr/bin/env python3
"""Machine-check the floating-point findings in the annotated release-binary listings.

Usage: check_listings.py LISTING_DIR EXCERPT_DIR
Reads <pin>-<platform>-{subsidy,kgw,ccbh}.s written by xref.py for the six shipped
platforms of both pins, asserts the findings of designs/CS2-fp-release-binaries.md, and
writes the FP-relevant lines of each listing (with 3 lines of context) to EXCERPT_DIR.
Exit status 0 only if every assertion holds.
"""
import os
import re
import sys

# platform -> (instruction set, KGW horizon shape, how pow is reached)
PLATFORMS = {
    "x86_64": ("x86", "separate", "plt"),          # Linux, Guix GCC
    "aarch64": ("arm64", "fused", "plt"),          # Linux, Guix GCC
    "riscv64": ("riscv", "fused", "plt"),          # Linux, Guix GCC
    "win64": ("x86", "separate", "static"),        # mingw-w64 GCC, pow linked statically
    "x86_64-darwin": ("x86", "separate", "plt"),   # macOS, clang, Apple libm through __stubs
    "arm64-darwin": ("arm64", "fused", "plt"),     # macOS, clang, Apple libm through __stubs
}

FP = {
    "x86": re.compile(r"\b(addsd|subsd|mulsd|divsd|sqrtsd|cvttsd2si|cvtsi2sd|comisd|ucomisd|andpd|vfn?m(add|sub)\w*)\b"),
    "arm64": re.compile(r"\b(fadd|fsub|fmul|fnmul|fdiv|fsqrt|fabd|fabs|fcmpe?|fccmpe?|f(n)?m(add|sub)|fcvtz[su]|[su]cvtf)\b"),
    "riscv": re.compile(r"\b(f(add|sub|mul|div|sqrt|min|max|sgnj|sgnjn|sgnjx|abs|neg|eq|lt|le)\.d|fn?m(add|sub)\.d|fcvt\.\w+\.\w+)\b"),
}
FMA = {
    "x86": re.compile(r"\bvfn?m(add|sub)\w*\b"),
    "arm64": re.compile(r"\bfn?m(add|sub)\b"),
    "riscv": re.compile(r"\bfn?m(add|sub)\.d\b"),
}
SQUARE = {
    "x86": re.compile(r"\bmulsd\s+(xmm\d+), \1$"),
    "arm64": re.compile(r"\bfmul\s+(d\d+), \1, \1$"),
    "riscv": re.compile(r"\bfmul\.d\s+\w+, (\w+), \1$"),
}
TRUNC = {
    "x86": re.compile(r"\bcvttsd2si\b"),
    "arm64": re.compile(r"\bfcvtzs\b"),
    "riscv": re.compile(r"\bfcvt\.l\.d\b.*\brtz$"),
}
ABS = {
    "x86": re.compile(r"\bandpd\b.*nan \(0x7fffffffffffffff\)"),
    "arm64": re.compile(r"\bfabd\b"),
    "riscv": re.compile(r"\bfabs\.d\b|\bfsgnjx\.d\s+(\w+), (\w+), \2$"),
}
X87 = re.compile(r"\b(fld|fstp|fild|fistp|fmulp|faddp|fdivp|fxch)\b")

failures = []


def check(cond, msg):
    print(("PASS " if cond else "FAIL ") + msg)
    if not cond:
        failures.append(msg)


def lines(path):
    with open(path) as fh:
        return fh.read().splitlines()


def excerpt(src, dst, isa):
    body = lines(src)
    keep = set()
    for i, l in enumerate(body):
        if FP[isa].search(l) or "pow@plt" in l or ("f64 " in l and "str '" not in l):
            keep.update(range(max(1, i - 3), min(len(body), i + 4)))
    out = [body[0]]
    prev = 0
    for i in sorted(keep):
        if i != prev + 1:
            out.append("      ...")
        out.append(body[i])
        prev = i
    with open(dst, "w") as fh:
        fh.write("\n".join(out) + "\n")


def call_target(line):
    m = re.search(r"<fn (0x[0-9a-f]+)", line)
    return m.group(1) if m else None


def main():
    src, dst = sys.argv[1], sys.argv[2]
    os.makedirs(dst, exist_ok=True)
    for pin in ("v23", "v24"):
        for plat, (isa, shape, powkind) in PLATFORMS.items():
            tag = f"{pin}-{plat}"
            sub = lines(f"{src}/{tag}-subsidy.s")
            kgw = lines(f"{src}/{tag}-kgw.s")
            ccbh = lines(f"{src}/{tag}-ccbh.s")
            darwin = plat.endswith("darwin")

            # KGW first: on win64 it identifies the statically linked pow.
            ktext = "\n".join(kgw)
            if darwin and isa == "arm64":
                anchored = "#0xbff3, lsl #48" in ktext and "#0x3fe6, lsl #48" in ktext  # -1.228, 0.7084 materialised
            else:
                anchored = "f64 -1.228 " in ktext and "f64 0.7084 " in ktext
            check(anchored, f"{tag} kgw: anchored (-1.228 and 0.7084 constants)")
            if powkind == "plt":
                pow_calls = [l for l in kgw if "<pow@plt>" in l]
                pow_marker = "<pow@plt>"
            else:
                # The last call before `mulsd ..., 0.7084` is the routine KGW passes x and -1.228 to.
                k = next(i for i, l in enumerate(kgw) if "mulsd" in l and "f64 0.7084 " in l)
                callee = call_target(next(l for l in reversed(kgw[:k]) if re.search(r"\bcall\b", l)))
                pow_marker = f"<fn {callee}>"
                pow_calls = [l for l in kgw if pow_marker in l]
                print(f"     {tag}: statically linked pow at {callee}")
            check(len(pow_calls) == 1, f"{tag} kgw: exactly one call to pow")
            if shape == "separate":
                check(not any(FMA[isa].search(l) for l in kgw), f"{tag} kgw: no fused multiply-add (separate rounding)")
                check(any("mulsd" in l and "f64 0.7084 " in l for l in kgw), f"{tag} kgw: separate mulsd by 0.7084")
            else:
                fused = [l for l in kgw if FMA[isa].search(l)]
                check(len(fused) == 1, f"{tag} kgw: 1+0.7084*pow(...) contracted to one fused multiply-add ({len(fused)} found)")
            if isa == "x86":
                check(not any(X87.search(l) for l in kgw), f"{tag} kgw: no x87")

            # Subsidy (GetBlockSubsidyHelper): anchored by the isDevnet assert.
            text = "\n".join(sub)
            check("'isDevnet'" in text, f"{tag} subsidy: anchored (isDevnet assert string)")
            check(pow_marker not in text and "<pow@plt>" not in text, f"{tag} subsidy: no call to pow")
            squares = [l for l in sub if SQUARE[isa].search(l)]
            truncs = [l for l in sub if TRUNC[isa].search(l)]
            if plat == "aarch64":
                # GCC tail-merges formulas 2 and 3: formula 2 loads 51.0/6.0/11111.0 and
                # branches to formula 3's "fdiv; fmul x,x; fdiv; fcvtzs" tail.
                check(len(squares) == 2, f"{tag} subsidy: pow(x, 2.0) compiled to x*x at 2 sites, formulas 2 and 3 sharing one ({len(squares)} found)")
                check(len(truncs) == 2, f"{tag} subsidy: 2 truncating double->int64 conversions, one shared")
                addr = {int(l.split(":")[0], 16): i for i, l in enumerate(sub[1:], 1) if re.match(r"\s*[0-9a-f]+:", l)}
                f2 = next(i for i, l in enumerate(sub) if re.search(r"movk\s+x\d+, #0x4049, lsl #48", l))
                br = next(l for l in sub[f2:f2 + 12] if re.search(r"\bb\s+#0x", l))
                tgt = addr[int(re.search(r"#0x([0-9a-f]+)", br).group(1), 16)]
                tail = [l for l in sub[tgt:tgt + 16] if FP[isa].search(l)][:4]
                ok = re.search(r"\bfdiv\b", tail[0]) and SQUARE[isa].search(tail[1]) and re.search(r"\bfdiv\b", tail[2]) and TRUNC[isa].search(tail[3])
                check(bool(ok), f"{tag} subsidy: formula 2 (51.0 materialised) branches into the shared div/square/div/fcvtzs tail")
            else:
                check(len(squares) == 3, f"{tag} subsidy: pow(x, 2.0) compiled to x*x at 3 sites ({len(squares)} found)")
                check(len(truncs) == 3, f"{tag} subsidy: 3 truncating double->int64 conversions ({len(truncs)} found)")
            check(not any(FMA[isa].search(l) for l in sub), f"{tag} subsidy: no fused multiply-add")
            if isa == "x86":
                check(not any(X87.search(l) for l in sub), f"{tag} subsidy: no x87")

            # Tolerance check (ContextualCheckBlockHeader, mainnet h <= 68589).
            check(any(ABS[isa].search(l) for l in ccbh), f"{tag} tolerance: abs is the double fabs")
            check(not any(FMA[isa].search(l) for l in ccbh), f"{tag} tolerance: no fused multiply-add")
            check(pow_marker not in "\n".join(ccbh), f"{tag} tolerance: no call to pow")

            for kind in ("subsidy", "kgw", "ccbh"):
                excerpt(f"{src}/{tag}-{kind}.s", f"{dst}/{tag}-{kind}.fp.s", isa)
    print(f"\n{len(failures)} failure(s)")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
