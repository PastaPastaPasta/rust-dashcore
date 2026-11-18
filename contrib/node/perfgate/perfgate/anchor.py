"""The fixed anchor for the drift guard (DEC-52 3).

Each gate compares the head with the previous integration head, so small allowed regressions could add up.
The anchor fixes, per workload, what was measured (the declaration's key and a hash of the package's bench
inputs), the iteration count and the median of every counter, at one revision: the last milestone, or the
last revision the manager re-anchored to. Every gate also compares its head with the anchor; cumulative
drift beyond 2% blocks (gate.drift_rows) until the manager re-anchors with a written justification:

    perfgate anchor <rev> --justification "M0 exit" [--only <glob>]

The current anchor is $PERFGATE_ROOT/anchor.json; every anchor ever recorded is appended to anchors.jsonl
with the drift it forgave (copy it into the committed ledger with the gate lines, DEC-52 2). Anchored
iteration counts are reused by every gate, so counts stay comparable across integration heads.

A toolchain, host, shim or counting-path change makes the old anchor's numbers incomparable. Re-anchoring then *carries*
the drift accumulated so far: the last authoritative gate of the same tree under the old fingerprint gives
each counter's drift d, and the new anchor stores measured / (1 + d), so the change resets nothing. A counter
that fell to 0 (d = -100%) or rose from 0 (no ratio, d = None) cannot be carried: its measured median is stored
and logged as uncarried, so its drift restarts from that median (BL-313).
"""

import json
import os
import statistics
import time

from . import paths


def path():
    return os.path.join(paths.root(), "anchor.json")


def load():
    try:
        with open(path()) as f:
            return json.load(f)
    except FileNotFoundError:
        return None


def iters(anchor):
    return {n: w["iters"] for n, w in (anchor or {}).get("workloads", {}).items() if "iters" in w}  # not REFUSED


def medians(result):
    return {k: statistics.median(v) for k, v in result["samples"].items() if v}


def noise(result):
    """Each counter's noise floor over the anchored runs (trimmed range): the anchor's own sampling error,
    which the drift limit allows for."""
    from .gate import trim

    return {k: max(trim(v)) - min(trim(v)) for k, v in result["samples"].items() if v}


def drift(old_medians, new_medians):
    """{counter: relative change} of `new_medians` against `old_medians`."""
    return {k: v / old_medians[k] - 1 for k, v in new_medians.items() if old_medians.get(k)}


def _carry(old_medians, new_medians):
    return drift(old_medians, new_medians) | {k: None for k, v in new_medians.items()
                                              if k in old_medians and not old_medians[k] and v}


def carried_drift(old, tree):
    """Drift of `tree` against the anchor `old` under the old fingerprint, from the last authoritative gate
    that measured exactly that tree: {workload: {counter: d}}, or None when there is no such gate. A counter
    anchored at 0 that the gate measured above 0 has no ratio: its d is None."""
    try:
        with open(os.path.join(paths.root(), "ledger.jsonl")) as f:
            entries = [json.loads(line) for line in f if line.strip()]
    except FileNotFoundError:
        return None
    for e in reversed(entries):
        if e.get("authoritative") and e.get("measured_tree") == tree and e.get("anchor") == old["rev"]:
            try:
                with open(os.path.join(e["report"], "head.json")) as f:
                    head = json.load(f)
            except OSError:
                continue
            return {n: _carry(old["workloads"][n]["medians"], medians(w)) for n, w in head["workloads"].items()
                    if w.get("status") == "ok" and "medians" in old["workloads"].get(n, {})}  # not REFUSED
    return None


def forgiven_drift(old, m):
    """The drift a re-anchor of `m` forgives against the comparable anchor `old`: {workload: {counter: d}}. A
    REFUSED record has no medians, so there is nothing to forgive (PF0-N §7)."""
    out = {}
    for n, w in m["workloads"].items():
        if w.get("status") == "ok" and "medians" in old["workloads"].get(n, {}):
            d = {k: v for k, v in drift(old["workloads"][n]["medians"], medians(w)).items() if abs(v) > 1e-4}
            if d:
                out[n] = d
    return out


PREFLIGHT_FIELDS = ("argv_sha256", "refused_on", "attempts", "accepted_by", "version")  # node-bench's preflight record


def _preflight(w):
    return {k: (w.get("preflight") or {}).get(k) for k in PREFLIGHT_FIELDS}


def record(m, justification, tree, selection, keys, inputs, merge_into=None, carried=None, forgiven=None,
           reset=None):
    """Anchor the measured workloads of `m` (`keys`: {workload: declaration key}, `inputs`: {workload: its bench
    inputs hash, or {"inputs", and for a node row its "components" and "tools" (PF0-N §8, D48)}}). A REFUSED node
    row (or a PREFLIGHT-ONLY one, D92) is recorded as such, with its preflight record and no medians (D21). With
    `merge_into` (a partial re-anchor whose fingerprint matches), the other workloads keep their anchored values;
    `carried` ({workload: {counter: d}}) is folded into the stored medians, except a counter whose d is None or
    <= -100% (it rose from 0 or fell to 0): that one is stored at its measured median, so its drift restarts there
    and an earlier improvement is not banked, and is listed under the workload's "uncarried". Gates read neither
    "carried" nor "uncarried"; both are the audit trail.

    Each anchors.jsonl line has "forgiven" (a plain re-anchor's forgiven drift), "carried" (only the counters
    applied to the workloads measured here), "uncarried" and "reset" (the drift a --reset-drift discarded although
    a gate could carry it), each {workload: {counter: d}} and {} when empty."""
    workloads = dict((merge_into or {}).get("workloads", {}))
    for name, w in m["workloads"].items():
        ins = inputs.get(name)
        ins = ins if isinstance(ins, dict) else {"inputs": ins}  # a bench row's inputs hash alone
        if w.get("status") in ("refused", "preflight-only"):
            workloads[name] = {"status": w["status"], "kind": w.get("kind"), "key": keys.get(name), **ins,
                               "preflight": _preflight(w)}
            continue
        if w.get("status") != "ok":
            continue
        meds = medians(w)
        d = (carried or {}).get(name, {})
        # A counter that fell to 0 against the old anchor (d = -100%) or rose from 0 (d None) has no ratio to
        # carry: its measured median is stored as is, and the record says it was not carried (BL-313).
        uncarried = {k: d[k] for k in meds if k in d and (d[k] is None or 1 + d[k] <= 0)}
        d = {k: v for k, v in d.items() if k not in uncarried}
        workloads[name] = {"iters": w["iters"], "kind": w.get("kind"), "key": keys.get(name), **ins,
                           "medians": {k: v / (1 + d.get(k, 0.0)) for k, v in meds.items()},
                           "noise": noise(w),
                           "carried": {k: d[k] for k in meds if k in d}}
        if uncarried:
            workloads[name]["uncarried"] = uncarried
        if w.get("preflight"):  # an ok node row also records its key and argv (PF0-N §7)
            workloads[name]["preflight"] = _preflight(w)
    meta = m["meta"]
    entry = {"rev": meta["rev"], "ref": meta.get("ref"), "tree": tree, "fingerprint": meta["fingerprint"],
             "time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "justification": justification,
             "selection": selection, "workloads": workloads}
    tmp = f"{path()}.{os.getpid()}"
    with open(tmp, "w") as f:
        json.dump(entry, f, indent=1)
    os.replace(tmp, path())
    with open(os.path.join(paths.root(), "anchors.jsonl"), "a") as f:
        f.write(json.dumps({k: v for k, v in entry.items() if k != "workloads"}
                           | {"workloads": sorted(m["workloads"]), "forgiven": forgiven or {},
                              "carried": {n: workloads[n]["carried"] for n in m["workloads"]
                                          if (workloads.get(n) or {}).get("carried")} if carried else {},
                              "uncarried": {n: workloads[n]["uncarried"] for n in m["workloads"]
                                            if "uncarried" in (workloads.get(n) or {})},
                              "reset": reset or {}},
                           sort_keys=True) + "\n")
    return entry
