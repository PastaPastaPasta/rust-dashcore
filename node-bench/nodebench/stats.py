"""Descriptive statistics and the bootstrap used by the comparison gate."""

import math
import random


def percentile(sorted_xs, q):
    """Linear-interpolated percentile, q in [0, 100]; input must be sorted."""
    if not sorted_xs:
        return None
    k = (len(sorted_xs) - 1) * q / 100
    lo, hi = math.floor(k), math.ceil(k)
    return sorted_xs[lo] + (sorted_xs[hi] - sorted_xs[lo]) * (k - lo)


def describe(xs):
    xs = sorted(x for x in xs if x is not None)
    n = len(xs)
    if not n:
        return {"n": 0}
    mean = sum(xs) / n
    sd = math.sqrt(sum((x - mean) ** 2 for x in xs) / (n - 1)) if n > 1 else 0.0
    med = percentile(xs, 50)
    mad = percentile(sorted(abs(x - med) for x in xs), 50)
    return {
        "n": n, "min": xs[0], "max": xs[-1], "mean": mean, "median": med, "stdev": sd,
        "cv": sd / mean if mean else None, "mad": mad,
        "p5": percentile(xs, 5), "p95": percentile(xs, 95), "p99": percentile(xs, 99),
    }


STATISTICS = {
    "median": lambda xs: percentile(sorted(xs), 50),
    "mean": lambda xs: sum(xs) / len(xs),
    "p95": lambda xs: percentile(sorted(xs), 95),
    "p99": lambda xs: percentile(sorted(xs), 99),
    "min": min,
    "max": max,
}


def bootstrap_ratio_ci(a, b, statistic="median", iters=1000, alpha=0.05, seed=1, cap=2000):
    """95% CI of stat(a)/stat(b) by independent resampling of both groups."""
    f = STATISTICS[statistic]
    rng = random.Random(seed)
    if len(a) > cap:
        a = rng.sample(a, cap)
    if len(b) > cap:
        b = rng.sample(b, cap)
    ratios = []
    for _ in range(iters):
        ra = f([rng.choice(a) for _ in a])
        rb = f([rng.choice(b) for _ in b])
        if rb:
            ratios.append(ra / rb)
    ratios.sort()
    return percentile(ratios, 100 * alpha / 2), percentile(ratios, 100 * (1 - alpha / 2))


def _midranks(values):
    order = sorted(range(len(values)), key=lambda i: values[i])
    ranks = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
            j += 1
        for k in range(i, j + 1):
            ranks[order[k]] = (i + j) / 2 + 1
        i = j + 1
    return ranks


EXACT_LIMIT = 200000


def mw_method(n, m, exact_limit=EXACT_LIMIT):
    """Which computation mann_whitney_greater uses for group sizes n, m ('exact' or 'normal')."""
    return "exact" if n and m and math.comb(n + m, n) <= exact_limit else "normal"


def mann_whitney_greater(x, y, exact_limit=EXACT_LIMIT):
    """One-sided Mann-Whitney U test of "x tends to be larger than y". Returns the p-value.

    Exact (enumerating every split of the pooled midranks, so ties are handled) when
    C(n+m, n) <= exact_limit; otherwise the normal approximation with tie and continuity
    correction.
    """
    n, m = len(x), len(y)
    if not n or not m:
        return None
    pooled = list(x) + list(y)
    ranks = _midranks(pooled)
    r_x = sum(ranks[:n])
    if math.comb(n + m, n) <= exact_limit:
        import itertools

        hits = total = 0
        for combo in itertools.combinations(ranks, n):
            total += 1
            if sum(combo) >= r_x - 1e-9:
                hits += 1
        return hits / total
    u = r_x - n * (n + 1) / 2
    mu = n * m / 2
    counts = {}
    for v in pooled:
        counts[v] = counts.get(v, 0) + 1
    ties = sum(t ** 3 - t for t in counts.values())
    var = n * m / 12 * ((n + m + 1) - ties / ((n + m) * (n + m - 1)))
    if var <= 0:
        return 1.0
    z = (u - mu - 0.5) / math.sqrt(var)
    return 0.5 * math.erfc(z / math.sqrt(2))
