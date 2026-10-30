"""Noise floors, the threshold policy (A1 §3.11, DEC-52) and the pass/fail report.

Round 1, for every (workload, counter), with 5 runs per side, each side trimmed of the one run farthest
from its median (`b'`, `h'`) so that a single stray run neither sets the threshold nor decides separation.
The noise floor is range(b'); the A1 threshold is max(min_rel x median(b), noise floor, absolute floor):

    REGRESSION  median(h) - median(b) > max(A1 threshold, 2 x noise floor; on a noisy base up to 2 x the
                head's trimmed range, bounded by 4 x noise floor), and either min(h') > max(b') or the base is
                deterministic (then a moved median is enough)
    IMPROVED    the mirror image (separation always required)
    NOISY       any other shift beyond the A1 threshold, or range(h') > max(2 x noise floor, min_rel x
                median, floor)
    OK          otherwise

NOISY is never a verdict (M1). A workload escalates when a counter is NOISY, or is a round-1 REGRESSION on
runs that were not bit-stable on both sides (five runs can miss a population's rare bursts): ESCALATE more
runs per side, interleaved. Then every gated counter of that workload is decided on all its runs (PF0 r2
ruling 3); a round-1 REGRESSION whose shift the pooled runs cannot settle stays blocking, as
NOISY-UNRESOLVED:

    threshold   the A1 threshold with the noise floor of all the base's runs (never the head's spread)
    shift       the larger of the Hodges-Lehmann estimate (median of all h_i - b_j) and the difference of the
                trimmed means: a regression that hits fewer than half the runs moves the mean, not the median
    REGRESSION  shift > threshold and the one-sided Mann-Whitney p (head > base) < ALPHA (exact: the
                permutation distribution of the rank sum, ties at their mid-rank)
    IMPROVED    both estimates < -threshold and p (head < base) < ALPHA
    NOISY-UNRESOLVED  either estimate above the threshold (towards worse) without significance: blocks
    OK          otherwise

Blocking: REGRESSION, NOISY-UNRESOLVED, DRIFT (cumulative drift against the anchor, DEC-52 3; a gated workload
or counter the anchor lacks is an ERROR), BENCH-CHANGED (bench inputs changed without recorded approval, M2)
and ERROR (a workload or counter that ran on the base but not the head, a base that failed to measure,
incomparable sides, an unusable anchor). ERROR takes precedence over FAIL in the verdict (exit 2).

Node workloads are not CPU-gated (DEC-52 4): only page faults, allocated bytes and peak RSS gate them.

A REGRESSION the manager has attributed and ruled on (BL-406) is APPROVED when an approval names its workload
and counter and its shift is within the approval's bound (`--approve-regression WORKLOAD:COUNTER:BOUND:DEC-n`,
BOUND a relative `+1.5%` or an absolute `+150` per run); above the bound it stays a REGRESSION. An approved row
is an anchor obligation (re-anchor it at the merge). No other status is ever approved this way: a
NOISY-UNRESOLVED shift was not attributed to anything, and DRIFT is judged against the anchor on its own.
"""

import math
import re
import statistics
from dataclasses import dataclass

MIN_REL = 0.005  # the default relative floor (A1 §3.11)
NOISE_K = 2
STABLE_TOL = 1e-4  # relative spread under which a counter counts as deterministic
ALPHA = 0.01  # one-sided Mann-Whitney level for escalated workloads
EXACT_MAX = 400  # runs in total up to which Mann-Whitney p-values are exact (every escalation today)
DRIFT_REL = 0.02  # cumulative drift against the anchor that blocks (DEC-52 3)


@dataclass(frozen=True)
class Counter:
    key: str
    label: str
    per_iter_floor: float = 0.0  # absolute floor, per iteration of the workload
    fixed_floor: float = 0.0  # absolute floor, per run
    gated: bool = True
    min_rel: float = MIN_REL  # relative floor; coarser counters raise it
    step_floor: float = 0.0  # replaces fixed_floor when the base's runs (untrimmed) spread by at least step_at
    step_at: float = 0.0


COUNTERS = [
    Counter("instructions", "instructions:u"),
    # From main() to the report: setup, and drops after the last span, which a bench edit could move out of
    # the region. Setup is often noisier (random keys, runtime start-up), hence the looser floor.
    Counter("instructions_total", "instructions:u, whole process", min_rel=0.01),
    # Event counts: at most one stray event per run is noise however few iterations a workload has (a 1-iteration
    # workload whose persistence timer fired once more must not block); 0.01 per iteration still catches a hot
    # loop going from 0 to 1 event per iteration.
    # The whole process tree under the sandbox, counted from outside it: the authoritative gated count (DEC-76
    # a), the one the code under test cannot report itself (it includes bwrap, dynamic loading and setup).
    Counter("instructions_process", "instructions:u, whole process, counted outside the sandbox", min_rel=0.01),
    Counter("allocs", "heap allocations", per_iter_floor=0.01, fixed_floor=1),
    Counter("alloc_bytes", "heap bytes allocated", per_iter_floor=1, fixed_floor=512),
    Counter("syscalls", "syscalls (excl. scheduling)", per_iter_floor=0.01, fixed_floor=1),
    Counter("fs_syscalls", "filesystem syscalls (open, stat, mkdir, rename, fsync, ...)", per_iter_floor=0.01,
            fixed_floor=1),
    Counter("read_bytes", "read bytes (/proc io rchar)", per_iter_floor=1, fixed_floor=512),
    Counter("write_bytes", "write bytes (/proc io wchar)", per_iter_floor=1, fixed_floor=512),
    Counter("read_syscalls", "read syscalls (/proc io syscr)", per_iter_floor=0.01, fixed_floor=1),
    Counter("write_syscalls", "write syscalls (/proc io syscw)", per_iter_floor=0.01, fixed_floor=1),
    Counter("page_faults", "page faults", per_iter_floor=0.01, fixed_floor=1),
    # Peak RSS is page- and allocator-granular. Where the base's own runs step by 0.5 MiB or more (the tokio
    # storage benches move between 49, 51, 52 and 55 MiB, ~1 MiB arena steps, with thread timing alone) growth
    # gates beyond 2% or 4 MiB; elsewhere beyond 2% or 256 KiB, so a small bench cannot double its RSS unnoticed.
    Counter("peak_rss_kib", "peak RSS KiB", fixed_floor=256, min_rel=0.02, step_floor=4096, step_at=512),
    Counter("sched_syscalls", "scheduling syscalls (futex, epoll_wait, ...)", gated=False),
]
BY_KEY = {c.key: c for c in COUNTERS}
# DEC-52 (4): node workloads gate memory signals only, until rdashd has a deterministic bench mode.
NODE_GATED = frozenset({"page_faults", "alloc_bytes", "peak_rss_kib"})
# Plan §10's INFO flags: the preload's peak RSS (VmHWM) against the trusted wait4 maxrss and spawn-k memory.peak.
RSS_MISMATCH_KIB = 4096
TREE_FLOOR_KIB = 65536
# m1 (Opus final): a node counter whose source changed (a custom allocator, memory the preload no longer sees)
# reads as a huge improvement; under this fraction of the reference it is an ERROR that needs approval.
SOURCE_FLOOR = 0.01
SOURCE_NOTE = "counter source changed"

BLOCKING = {"REGRESSION", "NOISY-UNRESOLVED", "DRIFT", "BENCH-CHANGED", "ERROR", "REFUSAL-REGRESSION",
            "NEW-FROM-REFUSED"}
# A node row's sides with a preflight record and no counts (PF0-N §7): refused, or accepted with no PF0 measured
# run yet (preflight-only, D92: a refusal of no options, so the §7 rules apply unchanged).
UNMEASURED = {"refused", "preflight-only"}
VERDICT_EXIT = {"PASS": 0, "FAIL": 1, "ERROR": 2, "INCOMPLETE": 3}

ROW_HEADER = ["| workload | counter | status | base | head | Δ | noise base / head | note |",
              "|---|---|---|---:|---:|---:|---|---|"]

POLICY = (f"round 1 (5 runs per side, one stray run dropped): a shift beyond max({MIN_REL:.1%}, {NOISE_K} x the "
          "base's noise floor, absolute floor) blocks when both sides repeated exactly; a separated shift on runs "
          "that spread, and any other shift beyond max(0.5%, noise floor, floor), escalate: 10 more runs per "
          "side, then every counter of the workload is decided on all its runs: REGRESSION if the larger of the "
          f"Hodges-Lehmann and trimmed-mean shifts exceeds the threshold and exact one-sided Mann-Whitney p < "
          f"{ALPHA}, NOISY-UNRESOLVED (blocks) if undecided; cumulative drift > {DRIFT_REL:.0%} against the "
          "anchor blocks; node workloads gate page faults, allocated bytes and peak RSS only")


@dataclass
class Row:
    workload: str
    counter: str
    status: str
    base: float = 0.0
    head: float = 0.0
    delta: float | None = None  # relative; None when the base median is 0
    noise_base: float | None = None
    noise_head: float | None = None
    allowed: float = 0.0
    iters: int = 1
    note: str = ""
    confirm: bool = False  # a round-1 REGRESSION on runs that were not bit-stable: escalation confirms it
    flags: tuple = ()  # a node row's plan §10 memory flags (INFO)


def trim(xs):
    """The runs without the one farthest from the median, when there are at least 5."""
    xs = list(xs)
    if len(xs) < 5:
        return xs
    m = statistics.median(xs)
    del xs[max(range(len(xs)), key=lambda i: abs(xs[i] - m))]
    return xs


def rel_range(xs):
    m = statistics.median(xs)
    spread = max(xs) - min(xs)
    if m == 0:
        return 0.0 if spread == 0 else None
    return spread / m


def mann_whitney_greater(base, head):
    """One-sided p that `head` is stochastically greater than `base`: the exact permutation distribution of
    the head's rank sum, with tied values sharing their mid-rank (counters tie often, and the normal
    approximation then overstates significance: [0]*15 vs [0]*10+[1]*5 is 0.0087 approximated, 0.0211 exact).
    Identical samples give 1. Beyond EXACT_MAX runs in total, the tie-corrected normal approximation."""
    n, m = len(base), len(head)
    pooled = sorted(base + head)
    big_n = n + m
    rank2, ties, i = {}, 0, 0  # doubled mid-ranks keep the sums integral
    while i < big_n:
        j = i
        while j < big_n and pooled[j] == pooled[i]:
            j += 1
        rank2[pooled[i]] = i + j + 1
        ties += (j - i) ** 3 - (j - i)
        i = j
    if ties == big_n ** 3 - big_n:
        return 1.0
    observed = sum(rank2[x] for x in head)
    if big_n > EXACT_MAX:
        var = n * m / 12 * ((big_n + 1) - ties / (big_n * (big_n - 1)))
        z = (observed / 2 - m * (big_n + 1) / 2 - 0.5) / math.sqrt(var)
        return 0.5 * math.erfc(z / math.sqrt(2))
    # ways[k][s]: subsets of k pooled values whose doubled mid-ranks sum to s
    ways = [dict() for _ in range(m + 1)]
    ways[0][0] = 1
    for x in pooled:
        r = rank2[x]
        for k in range(min(m, big_n) - 1, -1, -1):
            for total, count in ways[k].items():
                ways[k + 1][total + r] = ways[k + 1].get(total + r, 0) + count
    hits = sum(c for total, c in ways[m].items() if total >= observed)
    return hits / math.comb(big_n, m)


def hodges_lehmann(base, head):
    """The median of all pairwise differences head - base: a shift estimate that a few stray runs and a
    bimodal head do not drag around."""
    return statistics.median(h - b for h in head for b in base)


def floor_for(counter, iters, base_spread):
    fixed = counter.step_floor if counter.step_at and base_spread >= counter.step_at else counter.fixed_floor
    return max(counter.per_iter_floor * iters, fixed)


def judge(counter, base, head, iters, gated=None):
    """Round-1 status and numbers for one counter of one workload."""
    gated = counter.gated if gated is None else gated
    mb, mh = statistics.median(base), statistics.median(head)
    tb, th = trim(base), trim(head)
    rng_b, rng_h = max(tb) - min(tb), max(th) - min(th)
    floor = floor_for(counter, iters, max(base) - min(base))
    threshold = max(counter.min_rel * mb, rng_b, floor)  # A1 §3.11: max(noise floor, 0.5%)
    deterministic = rng_b <= STABLE_TOL * abs(mb)
    # What round 1 alone may call a regression: twice the noise floor, and on a noisy base up to twice the
    # head's spread (bounded). Shifts between `threshold` and this are NOISY and escalate, so the widening
    # only protects random-input workloads from 4-run flukes; it never lets a shift pass.
    allowed = max(threshold, NOISE_K * rng_b)
    if not deterministic:
        allowed = max(allowed, NOISE_K * min(rng_h, NOISE_K * rng_b))
    diff = mh - mb
    separated = min(th) > max(tb)
    note = ""
    confirm = False
    if not gated:
        status = "INFO"
    elif diff > allowed and (separated or deterministic):
        status = "REGRESSION"
        note = "" if separated else "the base is deterministic; the head's median moved and its runs spread"
        # Five runs can miss a population's rare bursts and look deterministic: unless both sides really
        # repeated, the regression is confirmed with more runs before it can fail a gate.
        confirm = not (deterministic and rng_h <= STABLE_TOL * abs(mh))
    elif -diff > allowed and max(th) < min(tb):
        status = "IMPROVED"
    elif abs(diff) > threshold or rng_h > max(NOISE_K * rng_b, counter.min_rel * mb, floor):
        status = "NOISY"
    else:
        status = "OK"
    return Row("", counter.key, status, mb, mh, diff / mb if mb else None, rel_range(tb), rel_range(th),
               allowed, iters, note, confirm)


def needs_escalation(row):
    """Round-1 rows that more runs must decide: NOISY, and regressions on runs that were not bit-stable."""
    return row.status == "NOISY" or (row.status == "REGRESSION" and row.confirm)


def trimmed_mean(xs):
    return statistics.fmean(trim(xs))


def judge_escalated(counter, base, head, iters, first):
    """The decision for a counter round 1 found NOISY, on the pooled runs (`first` round-1 runs, then the
    escalation's)."""
    # The noise floor from all the base's runs (it never depends on the head): 15 runs estimate it better than
    # round 1's 4 trimmed ones.
    tb = trim(base)
    mb, mh = statistics.median(base), statistics.median(head)
    threshold = max(counter.min_rel * mb, max(tb) - min(tb), floor_for(counter, iters, max(base) - min(base)))
    hl = hodges_lehmann(base, head)
    dmean = trimmed_mean(head) - trimmed_mean(base)
    p_up, p_down = mann_whitney_greater(base, head), mann_whitney_greater(head, base)
    note = (f"{len(base)} vs {len(head)} runs: HL {pct(hl / mb if mb else None)}, trimmed mean "
            f"{pct(dmean / mb if mb else None)}, p {min(p_up, p_down):.2g}")
    if max(hl, dmean) > threshold and p_up < ALPHA:
        status = "REGRESSION"
    elif min(-hl, -dmean) > threshold and p_down < ALPHA:
        status = "IMPROVED"
    elif max(hl, dmean) > threshold:  # an undecided shift towards worse; undecided improvements are OK
        status = "NOISY-UNRESOLVED"
    else:
        status = "OK"
    shift = max(hl, dmean) if status != "IMPROVED" else min(hl, dmean)
    return Row("", counter.key, status, mb, mh, shift / mb if mb else None, rel_range(tb),
               rel_range(trim(head)), threshold, iters, note)


def is_gated(counter, kind, ungated=()):
    """Whether `counter` blocks for a workload of `kind` whose declaration ungates `ungated`."""
    return counter.gated and (kind != "node" or counter.key in NODE_GATED) and counter.key not in ungated


def stable(result, tolerance=STABLE_TOL):
    """Whether every gated counter of a workload repeated (untrimmed) within `tolerance`: only such results
    may stand in for the base from the cache; the rest are re-run next to the head."""
    if result.get("status") != "ok":
        return False
    for c in COUNTERS:
        xs = result["samples"].get(c.key)
        if is_gated(c, result.get("kind"), result.get("ungated") or ()) and xs and \
                (rel_range(xs) is None or rel_range(xs) > tolerance):
            return False
    return True


# Fingerprint fields that must match for two measurements to be compared (Cargo.lock may differ: a PR
# may change dependencies; the worktree path differs by side).
COMPARABLE = ("harness", "counter", "rustc", "cargo", "cc", "cargo_config", "libc", "kernel", "cpu", "ncpu",
              "sandbox_label", "env")
# For an anchor, which outlives harness changes that do not touch the measurement: the shim and the counting
# path (measure.counter_hash), not the whole harness.
ANCHOR_COMPARABLE = ("shim", "counter", "rustc", "cargo", "cc", "cargo_config", "libc", "kernel", "cpu", "ncpu",
                     "sandbox_label", "env")


def fingerprint_diffs(a, b, fields):
    if not a or not b:
        return ["a fingerprint is missing"]
    return [f"{k} differs: {a.get(k)!r} vs {b.get(k)!r}" for k in fields if a.get(k) != b.get(k)]


def memory_flags(result):
    """Plan §10's INFO flags of one measured node side, run by run: `rss-mismatch` when the preload's VmHWM and the
    trusted wait4 maxrss (max over perf, bwrap and the node) differ by more than 4 MiB, `tree-memory` when spawn-k's
    memory.peak exceeds VmHWM by more than max(64 MiB, 50%) (a forked helper, a child without the preload, tmpfs)."""
    s = result.get("samples") or {}
    vm = s.get("peak_rss_kib") or []
    out = []
    for key, name, hit in (("maxrss_kib", "rss-mismatch", lambda v, x: abs(v - x) > RSS_MISMATCH_KIB),
                           ("memory_peak_kib", "tree-memory", lambda v, x: x > v + max(TREE_FLOOR_KIB, v / 2))):
        xs = s.get(key) or []
        if len(xs) != len(vm):  # a run without the broker's value: the runs can't be paired
            continue
        runs = [(v, x) for v, x in zip(vm, xs, strict=True) if hit(v, x)]
        if runs:
            v, x = max(runs, key=lambda r: abs(r[1] - r[0]))
            out.append(f"{name} in {len(runs)}/{len(vm)} runs (VmHWM {v} KiB, {key.removesuffix('_kib')} {x} KiB)")
    return out


def counter_source_changes(was, head, ungated=(), side="base"):
    """m1: {counter: why} for a node row whose counting source looks changed, against `side` (the base, or the
    anchor): a gated counter whose head median is under 1% of `was`'s, or `allocs` 0 on one side only (then the gated
    `alloc_bytes` too). `was` and `head` are {counter: median}; compare and drift share this one rule (D111)."""
    out = {}
    for k in sorted(NODE_GATED - set(ungated)):
        mw, mh = was.get(k), head.get(k)
        if mw and mh is not None and mh < SOURCE_FLOOR * mw:
            out[k] = f"the head's median is under {SOURCE_FLOOR:.0%} of the {side}'s"
    aw, ah = was.get("allocs"), head.get("allocs")
    if aw is not None and ah is not None and (aw == 0) != (ah == 0):
        why = f"allocs is 0 on the {side if aw == 0 else 'head'} only"
        out["allocs"] = why
        if "alloc_bytes" not in ungated:
            out.setdefault("alloc_bytes", why)
    return out


def medians(result):
    """{counter: median} of a measured side's samples."""
    return {k: statistics.median(v) for k, v in (result.get("samples") or {}).items() if v}


def source_obligations(rows):
    """The ledger's anchor obligations of approved counter-source changes (m1): one per row, naming its counters."""
    by_row = {}
    for r in rows:
        if r.status == "APPROVED" and r.note.startswith(SOURCE_NOTE):
            by_row.setdefault(r.workload, set()).add(r.counter)
    return [{"row": n, "reason": f"{SOURCE_NOTE} ({', '.join(sorted(cs))})"} for n, cs in sorted(by_row.items())]


@dataclass(frozen=True)
class RegressionApproval:
    """The manager's ruling on one attributed regression (BL-406): `workload`'s `counter` may regress by at most
    `bound`, relative (a fraction of the base median) when `relative`, else absolute per run; `ref` is the DEC."""
    workload: str
    counter: str
    bound: float
    relative: bool
    ref: str
    text: str


APPROVAL_FORM = re.compile(r"(?P<workload>[^:\s]+):(?P<counter>[a-z_]+):\+(?P<bound>[0-9]+(?:\.[0-9]+)?)(?P<pct>%?)"
                           r":(?P<ref>DEC-[0-9]+[a-z]?)")


def parse_regression_approval(text):
    """`WORKLOAD:COUNTER:+BOUND[%]:DEC-n` as a RegressionApproval; ValueError names what is wrong."""
    m = APPROVAL_FORM.fullmatch(text.strip())
    if not m:
        raise ValueError(f"not WORKLOAD:COUNTER:+BOUND[%]:DEC-n (e.g. node-startup-genesis:page_faults:+1.5%:DEC-464): "
                         f"{text!r}")
    if m["counter"] not in BY_KEY:
        raise ValueError(f"no counter {m['counter']!r} (one of {', '.join(sorted(BY_KEY))}): {text!r}")
    bound = float(m["bound"])
    if bound <= 0:
        raise ValueError(f"the bound must be above 0: {text!r}")
    relative = m["pct"] == "%"
    return RegressionApproval(m["workload"], m["counter"], bound / 100 if relative else bound, relative, m["ref"],
                              text.strip())


def _bound_text(a):
    return f"+{a.bound * 100:g}%" if a.relative else f"+{a.bound:g}"


def approve_regressions(rows, approvals):
    """Turn each REGRESSION an approval names into APPROVED when its shift is within the bound (BL-406); above it
    the row stays a REGRESSION, noting the bound. Returns the report's notes: one per approval no REGRESSION row
    used (a stale or mistyped approval is said, never silently ignored)."""
    by_key = {(a.workload, a.counter): a for a in approvals}
    used, seen = set(), {}
    for r in rows:
        a = by_key.get((r.workload, r.counter))
        if a is None:
            continue
        seen[a] = r.status
        if r.status != "REGRESSION":
            continue
        used.add(a)
        shift = r.delta * r.base if r.delta is not None else r.head - r.base
        rel = r.delta if r.delta is not None else math.inf
        # the shift is rebuilt from a rounded fraction: a shift equal to the ruled bound is within it
        value = rel if a.relative else shift
        if value <= a.bound or math.isclose(value, a.bound, rel_tol=1e-9):
            r.status = "APPROVED"
            r.note = (f"approved regression ({a.ref}, bound {_bound_text(a)}): shift {pct(r.delta)} ({human(shift)}); "
                      f"re-anchor {r.workload} at the merge" + (f"; {r.note}" if r.note else ""))
        else:
            r.note = (f"above the approved bound {_bound_text(a)} ({a.ref}): shift {pct(r.delta)} ({human(shift)})"
                      + (f"; {r.note}" if r.note else ""))
    return [f"approval {a.text} matched no REGRESSION row" + (f" (the row is {seen[a]})" if a in seen else "")
            for a in approvals if a not in used]


def regression_obligations(rows):
    """The ledger's anchor obligations of approved regressions (BL-406): one per row, naming its counters."""
    by_row = {}
    for r in rows:
        if r.status == "APPROVED" and r.note.startswith("approved regression"):
            by_row.setdefault(r.workload, set()).add(r.counter)
    return [{"row": n, "reason": f"approved regression ({', '.join(sorted(cs))})"} for n, cs in sorted(by_row.items())]


def _source_status(why, approval, after_merge):
    """A counter-source change: ERROR, or APPROVED with the manager's recorded approval (the row is then an anchor
    obligation)."""
    if approval:
        return "APPROVED", f"{SOURCE_NOTE}: {why}; approved: {approval}; {after_merge}"
    return "ERROR", (f"{SOURCE_NOTE}: {why} (a custom allocator, or memory the preload no longer sees): needs "
                     f"--approve-counter-source TEXT, then {after_merge}")


def compare(base_m, head_m, final=True, strict=False, source_approval=None):
    """Rows for every workload/counter in either measurement. With `final`, a NOISY row (a workload the gate
    could not escalate) is NOISY-UNRESOLVED. With `strict` (the harness is the base's own), a declared bench
    workload missing on the base is a declaration error: ERROR, not SKIPPED. A node row's counter whose source
    changed (m1) is an ERROR, or APPROVED with `source_approval`; a node row carries its plan §10 flags."""
    bf, hf = base_m["meta"].get("fingerprint"), head_m["meta"].get("fingerprint")
    rows = [Row("(sides)", "-", "ERROR", note=n) for n in (fingerprint_diffs(bf, hf, COMPARABLE) if bf and hf else [])]
    bw, hw = base_m["workloads"], head_m["workloads"]
    for name in list(bw) + [n for n in hw if n not in bw]:
        b, h = bw.get(name), hw.get(name)
        if b and b["status"] == "error":  # the head cannot be judged: never a silent pass
            rows.append(Row(name, "-", "ERROR", note=f"error on base: {b.get('error', '')}"))
            continue
        if h and h["status"] == "error":  # e.g. a refused node workload where the base has no binary yet
            rows.append(Row(name, "-", "ERROR", note=f"error on head: {h.get('error', '')}"))
            continue
        if {(b or {}).get("status"), (h or {}).get("status")} & UNMEASURED:
            continue  # refusal_rows (PF0-N §7)
        if strict and b and b["status"] == "missing":
            rows.append(Row(name, "-", "ERROR", note=f"declared but missing on the base: {b.get('error', '')}"))
            continue
        if not b or b["status"] != "ok":
            status = "NEW" if h and h["status"] == "ok" else "SKIPPED"
            note = (b or {}).get("error") or (b or {}).get("status", "absent") + " on base"
            rows.append(Row(name, "-", status, note=note))
            continue
        if not h or h["status"] != "ok":
            st = (h or {}).get("status", "absent")
            rows.append(Row(name, "-", "ERROR", note=f"{st} on head: {(h or {}).get('error', '')}".strip()))
            continue
        if b["iters"] != h["iters"]:
            rows.append(Row(name, "-", "ERROR", note=f"iteration counts differ ({b['iters']} vs {h['iters']})"))
            continue
        kind = b.get("kind") or h.get("kind")
        failed = [f"{side}: {r['escalation_error']}" for side, r in (("base", b), ("head", h))
                  if r.get("escalation_error")]
        if failed or b.get("escalated") != h.get("escalated"):
            rows.append(Row(name, "-", "ERROR", note="escalation runs failed (" + "; ".join(failed or ["one side"])
                            + "): the workload cannot be decided"))
            continue
        first = b.get("first_round") if b.get("escalated") else None
        ungated = b.get("ungated") or h.get("ungated") or ()
        source = counter_source_changes(medians(b), medians(h), ungated) if kind == "node" else {}
        start = len(rows)
        for c in COUNTERS:
            bs, hs = b["samples"].get(c.key), h["samples"].get(c.key)
            gated = is_gated(c, kind, ungated)
            if not bs:
                continue
            if c.key in source and hs:
                mb, mh = statistics.median(bs), statistics.median(hs)
                status, note = _source_status(source[c.key], source_approval,
                                              f"the manager re-anchors {name} after the merge")
                rows.append(Row(name, c.key, status, mb, mh, (mh - mb) / mb if mb else None, iters=b["iters"],
                                note=note))
                continue
            if not hs:
                if gated:
                    rows.append(Row(name, c.key, "ERROR", note="counted on the base, missing on the head"))
                continue
            # An escalated workload is decided on all its runs, every counter: later runs can contradict a
            # round-1 OK as well as confirm or clear a round-1 regression.
            row = judge_escalated(c, bs, hs, b["iters"], first) if first and gated else \
                judge(c, bs[:first] if first else bs, hs[:first] if first else hs, b["iters"], gated=gated)
            row.workload = name
            if not gated and row.status != "INFO":
                row.status = "INFO"
            if final and row.status == "NOISY":
                row.status = "NOISY-UNRESOLVED"
                row.note = (row.note + "; " if row.note else "") + "not escalated"
            if not gated and c.gated and kind == "node":
                row.note = "not CPU-gated (DEC-52 4)"
            elif not gated and c.gated:
                row.note = "ungated for this workload (workloads.toml gives the reason)"
            rows.append(row)
        flags = tuple(f"{side}: {f}" for side, r in (("base", b), ("head", h)) for f in memory_flags(r)) \
            if kind == "node" else ()
        if flags:
            row = next((r for r in rows[start:] if r.counter == "peak_rss_kib"), None)
            if row is None:
                row = Row(name, "-", "INFO")
                rows.append(row)
            row.flags = flags
            row.note = "; ".join(filter(None, (row.note, "plan §10 (INFO): " + "; ".join(flags))))
    return rows


def drift_rows(rows, head_m, anchor, current=None, base_m=None, source_approval=None):
    """Cumulative drift of the head against the fixed anchor (DEC-52 3): a gated counter whose head median
    exceeds the anchor's by more than max(2%, the comparison's threshold, the floor) blocks until the manager
    re-anchors with a written justification. `current` ({workload: {"key", "inputs"}}, the base's) makes an
    anchor that measured something else (another declaration, other bench inputs) an ERROR. A node row the base
    refuses is refusal_rows' (PF0-N §7); one the base measures against an unmeasured anchor record is an ERROR.
    A node row's counter under 1% of the anchor's (m1: its source changed, e.g. after an approved change merged) is
    an ERROR until re-anchored, or APPROVED with `source_approval`."""
    if not anchor:
        return [Row("(anchor)", "-", "ERROR",
                    note="no anchor recorded: run `perfgate anchor <rev> --justification ...`")]
    diffs = fingerprint_diffs(anchor.get("fingerprint"), head_m["meta"].get("fingerprint"), ANCHOR_COMPARABLE)
    if diffs:
        return [Row("(anchor)", "-", "ERROR", note=f"anchor {anchor['rev'][:12]} was recorded under another "
                    f"toolchain, host, shim or counting path ({'; '.join(diffs)}): re-anchor")]
    allowed = {(r.workload, r.counter): r.allowed for r in rows}
    out = []
    for name, h in head_m["workloads"].items():
        if h.get("status") != "ok":
            continue
        if ((base_m or {}).get("workloads", {}).get(name) or {}).get("status") in UNMEASURED:
            continue
        a = _anchored(anchor, name)
        if not a:
            out.append(Row(name, "-", "ERROR", note=f"not in the anchor: drift is unchecked; the manager anchors it "
                           f"(run anchor <rev> --only {name} --justification ...)"))
            continue
        if a.get("status") in UNMEASURED:
            out.append(Row(name, "-", "ERROR", note=f"anchor obligation: the base measures {name} but the anchor "
                           f"holds its {a['status'].upper()} record: run anchor <rev> --only {name} (PF0-N §7)"))
            continue
        if a["iters"] != h["iters"]:
            out.append(Row(name, "-", "ERROR", note=f"anchor iterations {a['iters']} vs {h['iters']}: re-anchor"))
            continue
        cur = (current or {}).get(name, {})
        stale = [what for what in ("key", "inputs") if cur.get(what) and a.get(what) != cur[what]]
        if stale:
            what = " and ".join(stale).replace("key", "declaration")
            if "inputs" in stale and cur.get("components"):  # a node row names its components (PF0-N D48)
                from .inputs import stale_components

                what = f"{what}: {', '.join(stale_components(a, cur))}"
            out.append(Row(name, "-", "ERROR", note=f"stale anchor ({what} changed since {anchor['rev'][:12]}): "
                           f"re-anchor {name}"))
            continue
        hm_all = medians(h)
        source = counter_source_changes(a["medians"], hm_all, h.get("ungated") or (), f"anchor {anchor['rev'][:12]}")\
            if h.get("kind") == "node" else {}
        for k, why in source.items():  # the row's other counters are still drift-checked below
            status, note = _source_status(why, source_approval, f"re-anchor {name} (run anchor <rev> --only {name})")
            out.append(Row(name, k, status, a["medians"].get(k, 0.0), hm_all.get(k, 0.0), iters=h["iters"], note=note))
        for c in COUNTERS:
            am, hs = a["medians"].get(c.key), h["samples"].get(c.key)
            if c.key in source or not hs or not is_gated(c, h.get("kind"), h.get("ungated") or ()):
                continue
            if am is None:
                out.append(Row(name, c.key, "ERROR", note=f"the anchor has no {c.key} for {name}: re-anchor it"))
                continue
            hm = statistics.median(hs)
            # The anchor's median has its own sampling error: its recorded noise floor widens the limit as the
            # comparison's own noise floor does.
            anchor_noise = NOISE_K * (a.get("noise") or {}).get(c.key, 0.0)
            limit = max(DRIFT_REL * am, allowed.get((name, c.key), 0.0), anchor_noise, floor_for(c, h["iters"], 0))
            if hm - am > limit:
                out.append(Row(name, c.key, "DRIFT", am, hm, (hm - am) / am if am else None, allowed=limit,
                               iters=h["iters"], note=f"since anchor {anchor['rev'][:12]} ({anchor.get('time', '')})"))
    return out


def _anchored(anchor, name):
    """The anchor's record of `name`; None when it has none (absent, or D69's pending placeholder)."""
    a = ((anchor or {}).get("workloads") or {}).get(name)
    return None if not a or a.get("status") == "pending" else a


# node-bench's refused_on entry for an RPC method the node lacks (`rpc:<method>`, owner PENDING; PF0-N D113).
METHOD_PREFIX = "rpc:"


def _options(rec):
    return [x["option"] for x in (rec or {}).get("refused_on") or ()]


def _compared(p, q):
    """The refused options of preflight records `p` and `q`, as sets to compare. A "settled" start never answered RPC,
    so it never probed the row's methods (D116): `rpc:` entries then count on neither side."""
    answered = "settled" not in (p.get("accepted_by"), q.get("accepted_by"))
    return tuple({o for o in _options(r) if answered or not o.startswith(METHOD_PREFIX)} for r in (p, q))


def _owned(rec):
    return ", ".join(f"{x['option']} ({x['owner']})" for x in (rec or {}).get("refused_on") or ()) or "none"


def _anchor_problem(name, bp, anchor):
    """D43 and D69: a REFUSED or PREFLIGHT-ONLY base row (its preflight record `bp`) against its anchor record, or
    None."""
    a = _anchored(anchor, name)
    if not a:
        return (f"not in the anchor: the manager anchors it (run anchor <rev> --only {name} --justification ...)")
    ap = a.get("preflight") or {}
    rev = str(anchor.get("rev", ""))[:12]
    if a.get("key") != bp.get("key"):
        return f"stale anchor (declaration changed since {rev}): re-anchor {name}"
    if ap.get("argv_sha256") != bp.get("argv_sha256"):
        return f"stale anchor (argv changed since {rev}): re-anchor {name}"
    measured = a.get("status") not in UNMEASURED
    ao, bo = _compared(ap, bp)
    ao = set() if measured else ao
    if not bo <= ao:
        return f"anchor inconsistent: base refuses {', '.join(sorted(bo - ao))} the anchor accepted"
    if measured:
        return f"anchor inconsistent: the anchor measured {name}, the base does not"
    if bo < ao:
        return (f"anchor obligation: `run anchor <tip> --only {name}` (refusals narrowed since {rev}: "
                f"{', '.join(sorted(ao - bo))} implemented)")
    if bp.get("refused_on") != ap.get("refused_on"):
        return f"anchor obligation: refusals changed (order or owners) since the anchor: was {_owned(ap)}, now " \
               f"{_owned(bp)}"
    return None


def _new_from_refused_problems(b, h, anchor_rec):
    """D20: why base REFUSED → head ok is not a first live measurement (empty when it is)."""
    bp, hp, ap = b["preflight"], h.get("preflight") or {}, anchor_rec.get("preflight") or {}
    out = []
    if not bp.get("key") == hp.get("key") == anchor_rec.get("key"):
        out.append("declaration changed")
    if not bp.get("argv_sha256") == hp.get("argv_sha256") == ap.get("argv_sha256"):
        out.append("argv changed")
    if bp.get("refused_on") != ap.get("refused_on"):
        out.append(f"refusals changed (order or owners) since the anchor: was {_owned(ap)}")
    argv = b.get("argv") or ()
    absent = [o for o in _options(bp) if not o.startswith(METHOD_PREFIX)  # a missing RPC method is no argv token
              and not any(t == o or t.startswith(o + "=") for t in argv)]
    if absent:
        out.append(f"refused option not in the argv: {', '.join(absent)}")
    if (hp.get("attempts"), hp.get("accepted_by")) != (1, "ready"):
        out.append(f"the head's preflight did not accept the argv as is ({hp.get('attempts')} attempts, "
                   f"{hp.get('accepted_by')})")
    return out


def refusal_rows(base_m, head_m, anchor, approval=None):
    """PF0-N §7 (D20, D41-D43, D62, D69): the rows of the node workloads REFUSED or PREFLIGHT-ONLY on either side,
    and the ledger fields {new_from_refused, refused_narrowed, anchor_obligations, unanchored_new (the rows the head
    measures first, with no anchor record: D22)}. compare() leaves these rows out;
    an error on either side stays its ERROR. PREFLIGHT-ONLY is a refusal of no options (D92): the node accepts the
    argv, and node-bench has no measured run for the row yet."""
    rows = []
    led = {"new_from_refused": [], "refused_narrowed": [], "anchor_obligations": [], "unanchored_new": []}
    bw, hw = base_m["workloads"], head_m["workloads"]
    for name in list(bw) + [n for n in hw if n not in bw]:
        b, h = bw.get(name) or {}, hw.get(name) or {}
        bs, hs = b.get("status"), h.get("status")
        if hs == "ok" and bs not in ("ok", "error", *UNMEASURED) and not _anchored(anchor, name):
            led["unanchored_new"].append(name)
        if not {bs, hs} & UNMEASURED or "error" in (bs, hs):
            continue

        def row(status, note, counter="-", head=0.0):
            rows.append(Row(name, counter, status, head=head, note=note))  # noqa: B023 - used in this iteration only

        if bs not in UNMEASURED:
            if bs == "ok" and hs == "refused":
                row("REFUSAL-REGRESSION",
                    f"the PR made rdashd refuse an option or RPC method the base accepted: {_owned(h['preflight'])}")
            elif bs == "pending":
                ho = _options(h["preflight"])
                row(hs.upper(), f"new binary, {'refused on ' + ', '.join(ho) if ho else 'argv accepted'}: anchor it "
                    "after the merge")
                led["anchor_obligations"].append({"row": name, "reason": f"new binary, {hs}"})
            else:
                row("ERROR", f"{bs or 'absent'} on base, {hs} on head")
            continue
        bp = b["preflight"]
        if hs == "pending":
            row("ERROR", "binary target removed (refused on the base)")
            continue
        if hs not in ("ok", *UNMEASURED) or (bs, hs) == ("preflight-only", "ok"):
            row("ERROR", f"{hs or 'absent'} on head ({bs} on the base)")
            continue
        problem = _anchor_problem(name, bp, anchor)
        if problem:
            row("ERROR", problem)
            continue
        if hs in UNMEASURED:  # D42
            hp = h["preflight"]
            bo, ho = _compared(bp, hp)
            if not ho <= bo:
                row("REFUSAL-REGRESSION", f"the head refuses {', '.join(sorted(ho - bo))}, which the base accepted")
            elif ho < bo:
                consumed = [x for x in bp["refused_on"] if x["option"] in bo - ho]
                row(hs.upper(), f"implemented: {', '.join(x['option'] for x in consumed)}; still refused: "
                    f"{_owned(hp)}")
                led["refused_narrowed"].append({"row": name, "key": bp.get("key"), "argv_sha256": bp.get("argv_sha256"),
                                                "consumed": consumed})
                led["anchor_obligations"].append({"row": name, "reason": "refusals narrowed"})
            elif hp.get("refused_on") != bp.get("refused_on"):
                row("REFUSED", f"refusals changed (order or owners): was {_owned(bp)}, now {_owned(hp)}")
                led["anchor_obligations"].append({"row": name, "reason": "owners changed"})
            elif hs == "refused":
                row("REFUSED", f"refused on {_owned(bp)}")
            else:
                row("PREFLIGHT-ONLY", "argv accepted; node-bench has no measured run for this row yet (D86)")
            continue
        a = anchor["workloads"][name]
        fails = _new_from_refused_problems(b, h, a)
        if fails:
            row("ERROR", "base refused, head measured, but not a first live measurement: " + "; ".join(fails))
            continue
        gated = {c.key: statistics.median(h["samples"][c.key]) for c in COUNTERS
                 if c.key in NODE_GATED and h.get("samples", {}).get(c.key)}
        status = "APPROVED" if approval else "NEW-FROM-REFUSED"
        why = f"approved: {approval}" if approval else "needs --approve-new-from-refused TEXT"
        note = f"first live measurement (implemented {', '.join(_options(bp))}); {why}"
        for k, v in gated.items():
            row(status, note, k, v)
        if not gated:  # still a row: the approval is the row's, not the counters'
            row(status, note)
        led["new_from_refused"].append({"row": name, "key": bp.get("key"), "argv_sha256": bp.get("argv_sha256"),
                                        "consumed": bp["refused_on"], "medians": gated, "approval": approval})
        led["anchor_obligations"].append({"row": name, "reason": "first live measurement"})
    return rows, led


def verdict(rows, complete=True):
    """ERROR (the gate could not judge something) takes precedence over FAIL (it judged a regression)."""
    if any(r.status == "ERROR" for r in rows):
        return "ERROR"
    if any(r.status in BLOCKING for r in rows):
        return "FAIL"
    return "PASS" if complete else "INCOMPLETE"


def flagged(rows):
    """The rows worth a reader's attention: everything but OK and INFO."""
    return [r for r in rows if r.status not in ("OK", "INFO")]


def report_json(rows, complete=True):
    return {"verdict": verdict(rows, complete), "rows": [r.__dict__ for r in rows]}


def human(x):
    for div, suf in ((1e9, "G"), (1e6, "M"), (1e3, "k")):
        if abs(x) >= div:
            return f"{x / div:.3f}{suf}"
    return f"{x:.3g}" if x != int(x) else str(int(x))


def pct(x):
    return "n/a" if x is None else f"{x * 100:+.3f}%"


def floor_pct(x):
    if x is None:
        return "∞"
    if x == 0:
        return "0"
    return f"{x * 100:.1e}%" if x < 1e-5 else f"{x * 100:.3f}%"


STATUSES = ("REGRESSION", "NOISY-UNRESOLVED", "DRIFT", "BENCH-CHANGED", "ERROR", "REFUSAL-REGRESSION",
            "NEW-FROM-REFUSED", "IMPROVED", "APPROVED", "NEW", "REFUSED", "PREFLIGHT-ONLY")


def render(rows, base_m, head_m, all_rows=False, notes=(), complete=True, title_verdict=None):
    """Markdown report: verdict, the policy, findings, then (optionally) every counter."""
    v = title_verdict or verdict(rows, complete)
    counts = {s: sum(r.status == s for r in rows) for s in STATUSES}
    bm, hm = base_m["meta"], head_m["meta"]
    out = [f"# PF0 counter gate: {v}", "",
           f"- base: `{bm.get('rev', '?')[:12]}` ({bm.get('label', '')}), head: `{hm.get('rev', '?')[:12]}` "
           f"({hm.get('label', '')}); {bm.get('repeat')} vs {hm.get('repeat')} runs per workload",
           "- " + ", ".join(f"{k.lower()}: {n}" for k, n in counts.items() if n or k in BLOCKING),
           f"- policy: {POLICY}",
           *(f"- **note:** {n}" for n in notes), ""]
    findings = flagged(rows)
    if findings:
        out += ROW_HEADER
        out += [_line(r) for r in findings]
        out.append("")
    mem = [r for r in rows if r.flags]
    if mem:
        out += ["## Node memory flags (INFO, plan §10)", "", *(f"- {r.workload}: {cell('; '.join(r.flags))}"
                                                               for r in mem), ""]
    out += ["## Per-workload instructions and noise floors", "",
            "| workload | iters | instr/iter base | Δ | instr noise base / head | max noise (other gated counters) |",
            "|---|---:|---:|---:|---|---|"]
    by_wl = {}
    for r in rows:
        by_wl.setdefault(r.workload, []).append(r)
    for wl, rs in by_wl.items():
        ins = next((r for r in rs if r.counter == "instructions"), None)
        if not ins:
            continue
        others = [r for r in rs if r.counter not in ("instructions", "-") and r.status != "INFO"]
        worst = max(others, key=lambda r: (r.noise_base is None, r.noise_base or 0), default=None)
        worst_s = f"{worst.counter} {floor_pct(worst.noise_base)}" if worst else "-"
        out.append(f"| {wl} | {ins.iters} | {human(ins.base / ins.iters)} | {pct(ins.delta)} | "
                   f"{floor_pct(ins.noise_base)} / {floor_pct(ins.noise_head)} | {worst_s} |")
    if all_rows:
        out += ["", "## All counters", "", *ROW_HEADER]
        out += [_line(r) for r in rows]
    return "\n".join(out) + "\n"


def cell(text):
    """`text` as one table cell: notes can carry a PR node's output (a preflight error), which must not add lines or
    columns to the report."""
    return "".join(c if c.isprintable() else c.encode("unicode_escape").decode("ascii")
                   for c in str(text)).replace("|", "\\|")


def _line(r):
    if r.counter == "-":
        return f"| {r.workload} | - | {r.status} | | | | | {cell(r.note)} |"
    return (f"| {r.workload} | {r.counter} | {r.status} | {human(r.base)} | {human(r.head)} | {pct(r.delta)} | "
            f"{floor_pct(r.noise_base)} / {floor_pct(r.noise_head)} | {cell(r.note)} |")


def noise_table(m):
    """Markdown noise floors of one measurement: the relative range of each counter over its runs."""
    keys = [c.key for c in COUNTERS]
    out = ["| workload | iters | runs | " + " | ".join(keys) + " |", "|---|---:|---:|" + "---:|" * len(keys)]
    for name, w in m["workloads"].items():
        if w["status"] != "ok":
            out.append(f"| {name} | | | {w['status']}: {cell(w.get('error', ''))} |")
            continue
        s = w["samples"]
        runs = len(s.get("instructions") or next(iter(s.values()), []))
        cells = [floor_pct(rel_range(trim(s[k]))) if s.get(k) else "-" for k in keys]
        out.append(f"| {name} | {w['iters']} | {runs} | " + " | ".join(cells) + " |")
    return "\n".join(out) + "\n"
