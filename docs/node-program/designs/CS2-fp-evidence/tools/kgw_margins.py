#!/usr/bin/env python3
"""Per-combination detail for the KGW floating-point gate (companion to kgw_analyze.py).

Prints, for every mass 14..4032 where the (d, s) pair differs between the
realised combinations, the bit patterns per combination; then the minimum
distance, over all masses and all integer gaps a in [1, 2^32-1], between
fl(150m/a) and each combination's d and s, in ulps. Also writes the correctly
rounded pow table (MPFR) used by the design's recommended implementation.
"""
import struct
import sys

import gmpy2
from gmpy2 import mpfr

sys.path.insert(0, __file__.rsplit("/", 1)[0])
from kgw_analyze import a_fast, a_slow, load, hx, A_MAX, SPACING  # noqa: E402


def ulp(x):
    return abs(float(gmpy2.next_above(mpfr(x))) - x)


def main():
    ctx = gmpy2.get_context()
    ctx.precision = 53
    ctx.round = gmpy2.RoundToNearest
    files = dict(arg.split("=", 1) for arg in sys.argv[1:])
    rows = {k: load(v) for k, v in files.items()}
    base = next(iter(rows.values()))

    cr = {m: float(mpfr(base[m]["x"]) ** mpfr(-1.228)) for m in range(1, 4033)}
    combos = {}
    for name, r in rows.items():
        for mode in ("sep", "fma"):
            combos[f"{name}+{mode}"] = {m: (r[m]["d_" + mode], r[m]["s_" + mode]) for m in range(1, 4033)}
    combos["cr+sep"] = {}
    combos["cr+fma"] = {}
    for m in range(1, 4033):
        p = cr[m]
        d_sep = 1.0 + 0.7084 * p
        d_fma = float(gmpy2.fma(mpfr(p), mpfr(0.7084), mpfr(1.0)))
        combos["cr+sep"][m] = (d_sep, 1.0 / d_sep)
        combos["cr+fma"][m] = (d_fma, 1.0 / d_fma)

    print("# masses (14..4032) where (d, s) differs between combinations")
    for m in range(14, 4033):
        vals = {c: v[m] for c, v in combos.items()}
        if len(set(vals.values())) > 1:
            print(f"m={m}: " + "; ".join(f"{c} d={hx(d)} s={hx(s)}" for c, (d, s) in sorted(vals.items())))

    print("\n# thresholds identical across all combinations?")
    same = True
    for m in range(14, 4033):
        ths = {(a_fast(m, d), a_slow(m, s)) for d, s in (v[m] for v in combos.values())}
        if len(ths) != 1:
            same = False
            print(f"  DIFFER at m={m}: {ths}")
    print("  yes" if same else "  NO")

    print("\n# minimum margin (ulps) between any fl(150m/a) and the boundary, per combination")
    for c, v in sorted(combos.items()):
        worst = None
        for m in range(14, 4033):
            d, s = v[m]
            t = float(SPACING * m)
            af, asl = a_fast(m, d), a_slow(m, s)
            for a, bound in ((af, d), (af + 1, d), (asl - 1, s), (asl, s)):
                if 1 <= a <= A_MAX:
                    u = abs(t / float(a) - bound) / ulp(bound)
                    if worst is None or u < worst[0]:
                        worst = (u, m, a)
        print(f"  {c:16s} min {worst[0]:12.1f} ulp at m={worst[1]}, a={worst[2]}")

    with open("kgw-pow-cr.txt", "w") as fh:
        for m in range(1, 4033):
            fh.write(f"{m} {hx(cr[m])}\n")
    print("\nwrote kgw-pow-cr.txt (correctly rounded pow(fl(m/28.2), fl(-1.228)), m = 1..4032)")


if __name__ == "__main__":
    main()
