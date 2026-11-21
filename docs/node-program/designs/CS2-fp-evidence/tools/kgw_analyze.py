#!/usr/bin/env python3
"""Analyse KGW's floating-point decisions across every libm/compiler combination.

Inputs: files written by kgw_ehd.c (one per libm variant). For each reachable
PastBlocksMass m (14..4032; the EHD is unused below PastBlocksMin = 14) and each
combination that a release dashd can realise, KGW breaks out of its loop iff

    ratio <= s  or  ratio >= d,   ratio = fl(150*m / a),  a = PastRateActualSeconds

with a in [1, 2^32-1] (a = 0 gives ratio = 1.0, which never breaks since
s < 1 < d). fl(150m/a) is non-increasing in a, so the break set is exactly
a <= A_fast(m) or a >= A_slow(m). If those two integer thresholds agree across
all combinations for every m, every combination takes the same decision on every
possible input, and KGW's output is identical (the rest of KGW is integer code).

Usage: kgw_analyze.py VARIANT=FILE[:sep|fma] ...
  e.g. x86-fma=out/ehd-x86-fma.txt:sep  (x86_64 release: mulsd+addsd)
       a64=out/ehd-a64.txt:fma          (aarch64 release: fmadd)
"""
import math
import struct
import sys

import gmpy2
from gmpy2 import mpfr

A_MAX = 2**32 - 1
SPACING = 150  # mainnet nPowTargetSpacing; KGW is reachable only on mainnet


def f(h):
    return struct.unpack("<d", struct.pack("<Q", int(h, 16)))[0]


def hx(d):
    return "%016x" % struct.unpack("<Q", struct.pack("<d", d))[0]


def load(path):
    rows = {}
    for line in open(path):
        m, x, p, dsep, dfma, ssep, sfma = line.split()
        rows[int(m)] = dict(x=f(x), p=f(p), d_sep=f(dsep), d_fma=f(dfma), s_sep=f(ssep), s_fma=f(sfma))
    return rows


def a_fast(m, d):
    """Largest a in [1, A_MAX] with fl(150m/a) >= d, or 0 if none."""
    t = float(SPACING * m)
    if t / 1.0 < d:
        return 0
    lo, hi = 1, A_MAX  # invariant: pred(lo) true
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if t / float(mid) >= d:
            lo = mid
        else:
            hi = mid - 1
    return lo


def a_slow(m, s):
    """Smallest a in [1, A_MAX] with fl(150m/a) <= s, or A_MAX+1 if none."""
    t = float(SPACING * m)
    if t / float(A_MAX) > s:
        return A_MAX + 1
    lo, hi = 1, A_MAX
    while lo < hi:
        mid = (lo + hi) // 2
        if t / float(mid) <= s:
            hi = mid
        else:
            lo = mid + 1
    return lo


def main():
    variants = []
    for arg in sys.argv[1:]:
        name, rest = arg.split("=", 1)
        path, mode = rest.rsplit(":", 1)
        variants.append((name, load(path), mode))

    # Correctly rounded reference (MPFR, round-to-nearest-even, 53 bits).
    ctx = gmpy2.get_context()
    ctx.precision = 53
    ctx.round = gmpy2.RoundToNearest
    y = -1.228
    base = variants[0][1]

    print("# pow(x_m, -1.228): variant vs correctly rounded (MPFR)")
    cr = {}
    for m in range(1, 4033):
        x = base[m]["x"]
        assert all(v[1][m]["x"] == x for v in variants), "x differs between variants"
        assert x == float(m) / 28.2
        cr[m] = float(mpfr(x) ** mpfr(y))
    for name, rows, _ in variants:
        bad = [m for m in range(1, 4033) if rows[m]["p"] != cr[m]]
        print(f"{name}: {len(bad)} of 4032 differ from CR; masses {bad[:20]}{' ...' if len(bad) > 20 else ''}")

    # Exact fma via MPFR as a cross-check of the C fma() results.
    for name, rows, _ in variants:
        for m in range(1, 4033):
            p = rows[m]["p"]
            exact_fma = float(gmpy2.fma(mpfr(p), mpfr(0.7084), mpfr(1.0)))
            assert exact_fma == rows[m]["d_fma"], (name, m)
            assert 1.0 + 0.7084 * p == rows[m]["d_sep"], (name, m)

    combos = [(name, rows, mode) for name, rows, mode in variants]
    # Add the CR-pow references with both combinations for completeness.
    cr_rows = {}
    for m in range(1, 4033):
        p = cr[m]
        d_sep = 1.0 + 0.7084 * p
        d_fma = float(gmpy2.fma(mpfr(p), mpfr(0.7084), mpfr(1.0)))
        cr_rows[m] = dict(p=p, d_sep=d_sep, d_fma=d_fma, s_sep=1.0 / d_sep, s_fma=1.0 / d_fma)
    combos.append(("cr+sep", cr_rows, "sep"))
    combos.append(("cr+fma", cr_rows, "fma"))

    print("\n# EHD (d, s) per combination; masses 14..4032 (used in decisions)")
    differing_m = []
    for m in range(14, 4033):
        ds = {(c[1][m]["d_" + c[2]], c[1][m]["s_" + c[2]]) for c in combos}
        if len(ds) > 1:
            differing_m.append(m)
    print(f"masses where (d, s) is not identical across combinations: {len(differing_m)}")

    print("\n# integer thresholds A_fast / A_slow per combination")
    divergent = []
    thresholds = {}
    for m in range(14, 4033):
        th = {}
        for name, rows, mode in combos:
            d = rows[m]["d_" + mode]
            s = rows[m]["s_" + mode]
            th[name] = (a_fast(m, d), a_slow(m, s))
        thresholds[m] = th
        if len(set(th.values())) > 1:
            divergent.append((m, th))
    print(f"masses with differing thresholds: {len(divergent)}")
    for m, th in divergent:
        print(f"  m={m}: {th}")
        for name, rows, mode in combos:
            print(f"     {name:10s} d={hx(rows[m]['d_' + mode])} s={hx(rows[m]['s_' + mode])}")

    # Margin: how close does any integer a come to flipping a decision?
    # For each m, the nearest ratio to d and to s, in ulps of d/s.
    print("\n# closest approach of fl(150m/a) to each boundary (in ulps of the boundary), reference combo =", combos[0][0])
    name, rows, mode = combos[0]
    worst = []
    for m in range(14, 4033):
        d = rows[m]["d_" + mode]
        s = rows[m]["s_" + mode]
        t = float(SPACING * m)
        af, asl = thresholds[m][name]
        cand = []
        for a in (af, af + 1):
            if 1 <= a <= A_MAX:
                r = t / float(a)
                cand.append(abs(r - d) / math.ulp(d))
        for a in (asl - 1, asl):
            if 1 <= a <= A_MAX:
                r = t / float(a)
                cand.append(abs(r - s) / math.ulp(s))
        worst.append((min(cand), m))
    worst.sort()
    for u, m in worst[:10]:
        print(f"  m={m}: {u:.3f} ulp")

    with open("kgw-thresholds.txt", "w") as fh:
        ref = combos[0][0]
        for m in range(14, 4033):
            af, asl = thresholds[m][ref]
            fh.write(f"{m} {af} {asl}\n")


if __name__ == "__main__":
    main()
