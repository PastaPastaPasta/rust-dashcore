#!/usr/bin/env python3
"""The CS2 KGW floating-point proof (design §2.4), as run on the evidence.

KGW breaks out of its loop at mass m (14..4032) and actual seconds a iff
    ratio <= s or ratio >= d,  ratio = fl(150m / a) (a > 0; a = 0 gives ratio 1.0),
    d = horizon(p) with p = pow(fl(m/28.2), fl(-1.228)), s = fl(1/d),
    horizon = fl(1 + fl(0.7084*p)) ("separate") or fl(1 + 0.7084*p) ("fused").
fl(150m/a) is non-increasing in a, so the break set is a <= A_fast or a >= A_slow.

Checks:
 1. Every measured shipped combination (a pow table plus a rounding shape) has the
    implementation's thresholds (the correctly rounded table, separate rounding).
 2. The two-endpoint check: thresholds at p_lo and p_hi equal the implementation's,
    for both shapes, where [p_lo, p_hi] contains every double within relative error
    EPS of the exact pow. Both horizon shapes are non-decreasing in p, s = fl(1/d) is
    non-increasing, and A_fast/A_slow are monotone in d/s, so equality at the two
    endpoints proves equality for every p in between: any pow implementation whose
    4032 results are within EPS reproduces Core's decisions, with either rounding.
 3. Margins: the smallest distance between any fl(150m/a) and any combination's d or s,
    and the largest relative pow error the decisions tolerate.

Usage: kgw_proof.py CR_TABLE [NAME=TABLE:SHAPE ...]
  TABLE files hold "m hex(x) hex(pow) ..." or "m hex(pow)" lines; SHAPE is sep or fused.
"""
import math
import struct
import sys

import gmpy2
from gmpy2 import mpfr

A_MAX = 2**32 - 1
SPACING = 150
EPS = 1e-9
WIDEN_ULPS = 4  # outward widening of each endpoint (absorbs the rounding of p_cr and of p_cr*(1±EPS))


def f(h):
    return struct.unpack("<d", struct.pack("<Q", int(h, 16)))[0]


def hx(d):
    return "%016x" % struct.unpack("<Q", struct.pack("<d", d))[0]


def load(path):
    out = {}
    for line in open(path):
        parts = line.split()
        out[int(parts[0])] = f(parts[2] if len(parts) >= 3 else parts[1])
    return out


def horizon(p, shape):
    if shape == "sep":
        return 1.0 + 0.7084 * p
    return float(gmpy2.fma(mpfr(p), mpfr(0.7084), mpfr(1.0)))  # exact single rounding


def a_fast(m, d):
    t = float(SPACING * m)
    if t / 1.0 < d:
        return 0
    lo, hi = 1, A_MAX
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if t / float(mid) >= d:
            lo = mid
        else:
            hi = mid - 1
    return lo


def a_slow(m, s):
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


def thresholds(m, p, shape):
    d = horizon(p, shape)
    return a_fast(m, d), a_slow(m, 1.0 / d)


def step(x, n):
    """x moved n ulps (positive x)."""
    return struct.unpack("<d", struct.pack("<Q", struct.unpack("<Q", struct.pack("<d", x))[0] + n))[0]


def endpoints(p, eps):
    return step(p * (1.0 - eps), -WIDEN_ULPS), step(p * (1.0 + eps), WIDEN_ULPS)


def main():
    ctx = gmpy2.get_context()
    ctx.precision = 53
    ctx.round = gmpy2.RoundToNearest
    cr = load(sys.argv[1])
    for m in range(1, 4033):  # the table must be MPFR's correctly rounded value
        assert cr[m] == float(mpfr(float(m) / 28.2) ** mpfr(-1.228)), m
    combos = []
    for arg in sys.argv[2:]:
        name, rest = arg.split("=", 1)
        path, shape = rest.rsplit(":", 1)
        combos.append((name, load(path), shape))

    impl = {m: thresholds(m, cr[m], "sep") for m in range(14, 4033)}

    print("# 1. measured shipped combinations vs the implementation (CR table, separate rounding)")
    for name, tab, shape in combos:
        bad = [m for m in range(14, 4033) if thresholds(m, tab[m], shape) != impl[m]]
        ndiff = sum(tab[m] != cr[m] for m in range(1, 4033))
        print(f"  {name:28s} {shape:5s}  pow differs from CR at {ndiff:4d} masses; threshold mismatches: {len(bad)}")
    dh = sorted({m for m in range(14, 4033)
                 if len({(horizon(t[m], sh), 1.0 / horizon(t[m], sh)) for _, t, sh in combos}) > 1})
    print(f"  masses (14..4032) where (d, s) differs between the measured combinations: {len(dh)}")
    print(f"    {dh}")

    print(f"\n# 2. two-endpoint check, EPS = {EPS:g}, endpoints widened by {WIDEN_ULPS} ulps")
    fails = []
    for m in range(14, 4033):
        lo, hi = endpoints(cr[m], EPS)
        for shape in ("sep", "fused"):
            for p in (lo, hi):
                if thresholds(m, p, shape) != impl[m]:
                    fails.append((m, shape, p))
    print(f"  {2 * 2 * (4033 - 14)} endpoint evaluations, failures: {len(fails)}")
    if fails:
        print("  first failures:", fails[:5])

    print("\n# 3. margins")
    worst = None
    for m in range(14, 4033):
        t = float(SPACING * m)
        af, asl = impl[m]
        ps = [cr[m]] + [tab[m] for _, tab, _ in combos]
        for p in ps:
            for shape in ("sep", "fused"):
                d = horizon(p, shape)
                s = 1.0 / d
                for a, bound in ((af, d), (af + 1, d), (asl - 1, s), (asl, s)):
                    if 1 <= a <= A_MAX:
                        u = abs(t / float(a) - bound) / math.ulp(bound)
                        if worst is None or u < worst[0]:
                            worst = (u, m, a)
    print(f"  smallest distance to a boundary: {worst[0]:.1f} ulps (m={worst[1]}, a={worst[2]})")
    # Largest EPS the endpoint check still passes (per mass, bisection on log scale).
    tol = None
    for m in range(14, 4033):
        lo_e, hi_e = 1e-12, 1e-6
        for _ in range(40):
            mid = math.sqrt(lo_e * hi_e)
            lo, hi = endpoints(cr[m], mid)
            ok = all(thresholds(m, p, sh) == impl[m] for p in (lo, hi) for sh in ("sep", "fused"))
            lo_e, hi_e = (mid, hi_e) if ok else (lo_e, mid)
        if tol is None or lo_e < tol[0]:
            tol = (lo_e, m)
    print(f"  largest relative pow error tolerated at every mass: {tol[0]:.3e} (binding mass m={tol[1]})")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
