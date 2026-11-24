#!/usr/bin/env python3
"""Mixed-regtest masternode soak (DEC-1893 (A)): one rdashd masternode among Core v24 masternodes.

  mn-soak.py run --out DIR [--seconds S] [--mn K] [--regular N] [--evo E] [--rdashd P] [--rdash-cli P]
  mn-soak.py summary DIR [--output NAME] [--rescan]
                                  # rewrite DIR/summary.md (or NAME) from DIR/cycles.jsonl and
                                  # DIR/meta.json; --rescan re-reads rdashd's log lines from its
                                  # debug.log and stderr (for runs drained with an older pattern)
  mn-soak.py nonrec --out DIR [--requests N] [--rdashd-send] [--mn K] [--rdashd P] [--rdash-cli P]
                                  # SG1-NR: feature_mn_nonrecovery.py, rdashd's share recovering a
                                  # signature while rdashd is a non-recovery member (case A) and a
                                  # ChainLock that needs it (case B); DIR/nonrec.json is the verdict
  mn-soak.py selftest

`run` exports func-compat's G2 test tree (the Core branch with rust_nodes.py's "mn:K" mask, at
the G2 release's dashd), copies feature_mn_soak.py into it and runs that one script directly, so
regular masternode K runs rdashd and every other node runs dashd. Build rdashd with
`--features rdashd/dkg-runtime` first (DEC-1881); a default rdashd has no DKG runtime.

Everything a run writes is under --out (mode 700): nodes/ (the datadirs, kept), test.log,
cycles.jsonl (one line per DKG cycle), meta.json and summary.md. One run at a time: the soak's
port range (PORT_MIN, --portseed) is held by its port lock. Never part of the default gate; run it by
hand or from a nightly unit, e.g.

  systemd-run --user --unit=ev1-mn-soak-1 --same-dir --collect -p RuntimeMaxSec=6h \\
    nice -n 10 contrib/node/mn-soak/mn-soak.py run --out /work/scratch/ev1/soak/run-1 --seconds 3600

Exit status: the soak script's (0 when every cycle ran to the deadline; flagged cycles are
evidence, not failures), 2 for a harness error, 75 when another soak holds the port lock.
"""

import argparse
import fcntl
import importlib.util
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
RDC = HERE.parents[2]
SOAK = "feature_mn_soak.py"
NONREC = "feature_mn_nonrecovery.py"
PORT_MIN = 12500  # p2p 20500+, rpc 30500+, tor 40500+ with PORT_SEED: clear of G2 (7300) and mixed-a (2700)
PORT_SEED = 400  # seeds n and n+1 are MAX_NODES (20) ports apart: two soaks on adjacent seeds don't overlap
PORT_LOCKS = Path(os.environ.get("MN_SOAK_PORT_LOCKS", "/work/scratch/mn-soak"))
LOCK_BUSY = 75
TYPE_NAMES = {100: "llmq_test", 102: "llmq_test_v17", 103: "llmq_test_dip0024", 106: "llmq_test_platform"}
ROTATION = 103
# llmq_test_platform (106) is evo-only, so rdashd, a regular masternode, is never in it.
RDASHD_TYPES = (100, 102, 103)
SIGN_TYPES = (100, 102, 103)
CYCLE = 24
WARMUP_MAX = 4  # Core's framework forms the rotation quorum within 3 cycles (mine_cycle_quorum)
TREND_MIN = 12  # samples after warm-up before a trend is judged
# Growth: the medians of the three thirds after warm-up rise in order, and for RSS the last
# exceeds the first by more than max(RSS_GROWTH_KB, RSS_GROWTH_FRAC of the first). Any ordered fd
# rise counts. The OLS slope is reported, not judged: it shrinks as a run gets longer.
RSS_GROWTH_KB = 5000
RSS_GROWTH_FRAC = 0.10
GROUP_NOISE = re.compile(r"^\S+ \S+ |\b[0-9a-f]{16,}\b|\b\d+(\.\d+)?\b")
# Same pattern as feature_mn_soak.LOG_PROBLEM: rdashd's GetLogPrefix forms and Core's text forms.
LOG_PROBLEM = re.compile(r"\[(?:[a-z0-9_.-]+:)?(?:warning|error)\] |\bERROR\b|\bWarning:|\bError:|\bwarning:|\berror:")
PANIC = re.compile(r"panicked at")
UPDATE_TIP = re.compile(r"UpdateTip: new best=\S+ height=(\d+)")
# Known-benign rdashd warning/error line groups, as (pattern, reason): a matching line is listed
# but doesn't flag its cycle. Empty: no group has been shown benign yet.
BENIGN_LOG = []


def func_compat():
    spec = importlib.util.spec_from_file_location("func_compat", HERE.parent / "func-compat/func-compat.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


# ---- the summary -------------------------------------------------------------------------------

def load(out):
    out = Path(out)
    meta = json.loads((out / "meta.json").read_text()) if (out / "meta.json").exists() else {}
    events = [json.loads(line) for line in (out / "cycles.jsonl").read_text().splitlines() if line.strip()]
    return meta, events


def rescan_logs(out, meta, cycles):
    """Re-read rdashd's debug.log with LOG_PROBLEM into each cycle's rdashd_log, placing a line in
    the cycle whose 24 blocks hold the last UpdateTip height before it. Returns the matching
    lines outside every cycle (setup, shutdown) and stderr's panic lines."""
    node = Path(out) / "nodes" / f"node{meta.get('rdashd_node')}"
    by_base = {c["base"]: [] for c in cycles}
    outside, height = [], -1
    with open(node / "regtest" / "debug.log", encoding="utf8", errors="replace") as f:
        for line in f:
            tip = UPDATE_TIP.search(line)
            if tip:
                height = int(tip.group(1))
            elif LOG_PROBLEM.search(line):
                by_base.get(height - height % CYCLE, outside).append(line.rstrip("\n")[:400])
    for c in cycles:
        c["rdashd_log"] = by_base[c["base"]]
    panics = []
    for path in sorted((node / "stderr").glob("*")):
        panics += ["stderr: " + line[:400] for line in path.read_text(errors="replace").splitlines() if PANIC.search(line)]
    return outside, panics


def significant(lines):
    return [line for line in lines if not any(re.search(p, line) for p, _ in BENIGN_LOG)]


def rotation_formed(c):
    return any(s and s["type"] == ROTATION and s.get("commitment") and not s["commitment"].get("null")
               for s in c.get("sessions", []))


def warmup_length(cycles):
    """The cycles up to and including the first non-null llmq_test_dip0024 commitment, at most
    WARMUP_MAX, and whether the rotation quorum formed within them."""
    for i, c in enumerate(cycles[:WARMUP_MAX]):
        if rotation_formed(c):
            return i + 1, True
    return min(WARMUP_MAX, len(cycles)), False


def session_flags(s, warmup=False):
    """Why a session is flagged: no DKG session or no non-null commitment for an enabled type
    (llmq_test_dip0024 exempt in the warm-up), a commitment the miner lacked, or, for rdashd,
    selected but not valid or not a signer, a phase that timed out, or a Core member that marked
    rdashd bad or complained about it."""
    flags = []
    if not s or s.get("enabled") is False:
        return flags
    selected, c = s.get("selected"), s.get("commitment")
    no_commitment = c is None or c.get("null")
    if not (warmup and s["type"] == ROTATION):
        if s.get("empty"):
            flags.append("no DKG session on any Core member")
        if no_commitment:
            flags.append("selected, no final commitment" if selected else "no non-null final commitment")
    elif selected and no_commitment:
        flags.append("selected, no final commitment")
    if (s.get("miner_wait") or {}).get("timed_out"):
        flags.append("miner lacked a commitment a Core member held")
    if not selected:
        return flags
    if s.get("rdashd_valid") is False:
        flags.append("selected, not valid in the final commitment")
    elif s.get("rdashd_signer") is False:
        flags.append("selected, not a signer of the final commitment")
    if s.get("rdashd_bad_by"):
        flags.append(f"marked bad by {s['rdashd_bad_by']} Core member(s)")
    if s.get("rdashd_complained_by"):
        flags.append(f"complained about by {s['rdashd_complained_by']} Core member(s)")
    late = [p for p, v in sorted(s.get("phases", {}).items()) if v.get("timed_out")]
    if late:
        flags.append(f"phase wait timed out: {','.join(map(str, late))}")
    return flags


def same_class(c, llmq_type, rnode):
    """The Core masternodes (the nodes `quorum sign` was asked on) in rdashd's class for
    `llmq_type`: active quorum members if rdashd is one, else non-members. Core relays plain
    recovered signatures only to quorum relay members."""
    members = (c.get("active_members") or {}).get(str(llmq_type))
    if not isinstance(members, list) or not rnode.isdigit():
        return None, []
    member = int(rnode) in members
    masternodes = set().union(*(s.get("asked", {}) for s in c.get("signs", []) if not s.get("dependent")))
    nodes = [k for k in (c.get("cl_recsigs") or {}).get("held", {})
             if k != rnode and (not masternodes or k in masternodes)]
    return member, [k for k in nodes if (int(k) in members) == member]


def recovered_on(sign, node):
    """Whether `node` held the recovered sig (a time; an RPC error string is not one)."""
    v = sign.get("recovered_after_s", {}).get(node)
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def sign_member(c, sign, rnode):
    """Whether rdashd is in the request's signing quorum (`quorum selectquorum`), falling back for
    records without it to the cycle's active quorums."""
    if "rdashd_member" in sign:
        return sign["rdashd_member"]
    return same_class(c, sign.get("type", 100), rnode)[0]


def sign_flags(c, rnode):
    """Differing recovered sigs; rdashd's `quorum sign` not true for a quorum it is in; an
    rdashd-dependent request that recovered nowhere; and rdashd, in the signing quorum, holding
    fewer recovered sigs than every Core member of it. Non-members are not compared: Core sends
    plain recovered sigs along the relay links of a quorum of any type (QSENDRECSIGS), so what a
    non-member holds depends on its other memberships (an evo node's platform quorums)."""
    flags = []
    for sign in c.get("signs", []):
        t = sign.get("type", 100)
        if sign.get("skipped"):
            continue
        if sign.get("distinct_sigs", 1) > 1:
            flags.append(f"type {t}: recovered signatures differ between nodes")
        member = sign_member(c, sign, rnode)
        asked = sign.get("asked", {})
        if sign.get("rdashd_member") and asked.get(rnode) is not True:
            flags.append(f"type {t}: quorum sign on rdashd returned {asked.get(rnode)!r} for a quorum it is in")
        if sign.get("dependent"):
            if not any(recovered_on(sign, k) for k in sign.get("recovered_after_s", {})):
                flags.append(f"type {t}: rdashd-dependent request recovered on no node")
            continue
        if "quorum_members" in sign:
            peers = [str(k) for k in sign["quorum_members"] if str(k) != rnode] if member else []
        else:
            peers = same_class(c, t, rnode)[1] if member else []
        if member and peers and all(recovered_on(sign, k) for k in peers) and not recovered_on(sign, rnode):
            flags.append(f"type {t}: rdashd, in the signing quorum, lacks a recovered sig every Core member holds")
    held = (c.get("cl_recsigs") or {}).get("held", {})
    member, peers = same_class(c, 100, rnode)
    if member and peers and rnode in held and held[rnode] < min(held[k] for k in peers):
        flags.append(f"ChainLock recsigs on rdashd {held[rnode]} < every Core member's ({min(held[k] for k in peers)})")
    return flags


def cycle_flags(c, rnode="-1", warmup=False):
    flags = []
    for s in c.get("sessions", []):
        name = f"{TYPE_NAMES.get(s['type'], s['type'])}#{s['index']}" if s else None
        flags += [f"{name}: {f}" for f in session_flags(s, warmup)]
    pose = c.get("pose") or {}
    if (pose.get("penalty") or 0) > 0:
        flags.append(f"PoSe penalty {pose['penalty']}")
    if pose.get("ban_height", -1) not in (-1, None):
        flags.append(f"PoSe banned at {pose['ban_height']}")
    cl = c.get("chainlock") or {}
    if not cl.get("agree", True) or cl.get("tip_locked_s") is None:
        flags.append("ChainLock disagreement or tip not locked")
    if (c.get("mempool_sync") or {}).get("timed_out"):
        flags.append("mempool sync timed out")
    if (c.get("islock_wait") or {}).get("timed_out") and not warmup:
        flags.append("InstantSend lock missing on a node")
    logs = significant(c.get("rdashd_log", []))
    if logs:
        flags.append(f"{len(logs)} rdashd warning/error/panic line(s)")
    return flags + sign_flags(c, rnode)


def trend(values):
    """The OLS slope per sample and the medians of the first, middle and last thirds of `values`."""
    vs = [v for v in values if v is not None]
    n = len(vs)
    if n < 3:
        v = statistics.median(vs) if vs else None
        return {"n": n, "slope": 0.0, "first_third": v, "middle_third": v, "last_third": v}
    xbar, ybar = (n - 1) / 2, sum(vs) / n
    slope = sum((i - xbar) * (v - ybar) for i, v in enumerate(vs)) / sum((i - xbar) ** 2 for i in range(n))
    third = n // 3
    return {"n": n, "slope": slope, "first_third": statistics.median(vs[:third]),
            "middle_third": statistics.median(vs[third:n - third]), "last_third": statistics.median(vs[-third:])}


def rises(t):
    return t["n"] >= TREND_MIN and t["first_third"] < t["middle_third"] < t["last_third"]


def rss_grows(t):
    return rises(t) and t["last_third"] - t["first_third"] > max(RSS_GROWTH_KB, RSS_GROWTH_FRAC * t["first_third"])


def fds_grow(t):
    return rises(t)


def signs_of(cycles, t):
    return [(c, s) for c in cycles for s in c.get("signs", []) if s.get("type", 100) == t]


def run_flags(cycles, warm, formed, rnode, outside=(), panics=()):
    """Flags over the whole run: rotation not formed in the warm-up, RSS or fd growth after it,
    rdashd lines outside every cycle or panics, and every metric measured on 0 samples."""
    flags = []
    if not formed and len(cycles) >= WARMUP_MAX:
        flags.append(f"rotation quorum not formed after {WARMUP_MAX} cycles")
    measured = cycles[warm:]
    rss = trend([(c.get("rdashd_proc") or {}).get("rss_kb") for c in measured])
    fds = trend([(c.get("rdashd_proc") or {}).get("fds") for c in measured])
    if rss_grows(rss):
        gain = rss["last_third"] - rss["first_third"]
        flags.append(f"rdashd RSS grows after warm-up: third medians {rss['first_third']} < {rss['middle_third']} < "
                     f"{rss['last_third']} kB (+{gain:.0f} kB, +{100 * gain / rss['first_third']:.0f}%)")
    if fds_grow(fds):
        flags.append(f"rdashd fds grow after warm-up: third medians {fds['first_third']} < {fds['middle_third']} < "
                     f"{fds['last_third']}")
    if 0 < rss["n"] < TREND_MIN:
        flags.append(f"RSS/fd trend judged on {rss['n']} samples (< {TREND_MIN})")
    if significant(outside):
        flags.append(f"{len(significant(outside))} rdashd warning/error line(s) outside the cycles (setup or shutdown)")
    if panics:
        flags.append(f"{len(panics)} rdashd stderr panic line(s)")
    zero = []
    if not measured:
        zero.append("InstantSend after warm-up")
    if not rss["n"]:
        zero.append("rdashd RSS and fds after warm-up")
    if not sum((c.get("cl_recsigs") or {}).get("heights", 0) for c in cycles):
        zero.append("ChainLock recovered signatures")
    for t in RDASHD_TYPES:
        name = TYPE_NAMES[t]
        ss = [s for c in cycles for s in c.get("sessions", []) if s and s["type"] == t and s.get("enabled", True)]
        if not ss:
            zero.append(f"{name} DKG sessions")
        elif not any(s.get("selected") for s in ss):
            zero.append(f"{name} sessions with rdashd selected")
    for t in SIGN_TYPES:
        name, signs = TYPE_NAMES[t], signs_of(cycles, t)
        if not any(not s.get("dependent") and sign_member(c, s, rnode) for c, s in signs):
            zero.append(f"{name} control signing with rdashd in the quorum")
        if not any(s.get("dependent") and not s.get("skipped") for _, s in signs):
            zero.append(f"{name} rdashd-dependent signing")
    return flags + [f"measured on 0 samples: {z}" for z in zero]


def group_log_lines(lines):
    groups = Counter(GROUP_NOISE.sub("#", line).strip() for line in lines)
    return groups.most_common()


def fmt_trend(name, t, unit, grows):
    if not t["n"]:
        return f"- {name}: no samples"
    return (f"- {name}: {t['n']} samples after warm-up, OLS slope {t['slope']:+.1f}{unit}/cycle, "
            f"third medians {t['first_third']}{unit} / {t['middle_third']}{unit} / {t['last_third']}{unit}; "
            f"growth flagged: {'YES' if grows else 'no'}")


def summarize(meta, events, rescanned=None):
    """The markdown summary. `rescanned` is rescan_logs()'s (outside, panics) when the cycles'
    rdashd_log was re-read from the node's debug.log."""
    cycles = [e for e in events if e.get("event") == "cycle"]
    ends = [e for e in events if e.get("event") != "cycle"]
    lines = ["# mn-soak summary", ""]
    if not cycles:
        return "\n".join(lines + ["No cycle completed.", *[f"- {json.dumps(e)}" for e in ends], ""])
    rnode = str(meta.get("rdashd_node"))
    outside, panics = rescanned or ([line for e in ends for line in e.get("rdashd_log", [])], [])
    first, last = cycles[0], cycles[-1]
    wall = sum(c.get("wall_s", 0) for c in cycles)
    warm, formed = warmup_length(cycles)
    lines += [f"- rdashd masternode: node {meta.get('rdashd_node')} ({meta.get('rdashd_protx')})",
              f"- network: {meta.get('regular')} regular masternodes (rdashd's included), {meta.get('evo')} Core evo masternodes",
              f"- cycles: {len(cycles)}, bases {first['base']}..{last['base']} ({last['base'] + CYCLE - first['base']} blocks), cycle wall time {wall / 60:.1f} min",
              f"- end: {', '.join(json.dumps({k: v for k, v in e.items() if k != 'rdashd_log'}, sort_keys=True) for e in ends) or 'none recorded (killed?)'}",
              f"- rdashd log lines: {'re-read from its debug.log and stderr (rescan)' if rescanned else 'as the soak drained them'}; "
              "cycles.jsonl holds every line", ""]
    lines += ["## Per LLMQ type", "",
              "| type | sessions | not enabled | rdashd selected | non-null commitment | rdashd valid | rdashd signer | flagged |",
              "|---|---|---|---|---|---|---|---|"]
    by_type = {}
    for i, c in enumerate(cycles):
        for s in c.get("sessions", []):
            if s:
                by_type.setdefault((s["type"], s["index"]), []).append((s, i < warm))
    for (t, i), ss in sorted(by_type.items()):
        on = [(s, w) for s, w in ss if s.get("enabled", True)]
        sel = [s for s, _ in on if s.get("selected")]
        lines.append(f"| {TYPE_NAMES.get(t, t)}#{i} | {len(on)} | {len(ss) - len(on)} | {len(sel)} | "
                     f"{sum(1 for s, _ in on if s.get('commitment') and not s['commitment'].get('null'))} | "
                     f"{sum(1 for s in sel if s.get('rdashd_valid'))} | {sum(1 for s in sel if s.get('rdashd_signer'))} | "
                     f"{sum(1 for s, w in on if session_flags(s, w))} |")
    flagged = [(c["base"], cycle_flags(c, rnode, i < warm)) for i, c in enumerate(cycles)]
    flagged = [(b, f) for b, f in flagged if f]
    runwide = run_flags(cycles, warm, formed, rnode, outside, panics)
    lines += ["", "## Flagged cycles", "", f"{len(flagged)} of {len(cycles)} cycles flagged; {len(runwide)} run-level flag(s).",
              f"Rotation warm-up: {warm} cycle(s) (bases {cycles[0]['base']}..{cycles[warm - 1]['base']}), "
              f"{'ended by the first llmq_test_dip0024 quorum' if formed else 'ended at the bound without one'}; "
              "its empty llmq_test_dip0024 sessions and missing IS locks are not flagged."]
    if runwide:
        lines += ["", "Run-level:"] + [f"- {f}" for f in runwide]
    if flagged:
        lines += ["", "Cycles:"] + [f"- base {b}: {'; '.join(f)}" for b, f in flagged]
    measured = cycles[warm:]
    rss = trend([(c.get("rdashd_proc") or {}).get("rss_kb") for c in measured])
    fds = trend([(c.get("rdashd_proc") or {}).get("fds") for c in measured])
    all_rss = [v for v in ((c.get("rdashd_proc") or {}).get("rss_kb") for c in cycles) if v is not None]
    lines += ["", "## rdashd memory and fds", "",
              fmt_trend("RSS", rss, " kB", rss_grows(rss)), fmt_trend("fds", fds, "", fds_grow(fds))]
    if all_rss:
        lines.append(f"- RSS over every cycle: min {min(all_rss)} kB, max {max(all_rss)} kB")
    cl_ok = sum(1 for c in cycles if (c.get("chainlock") or {}).get("agree") and (c["chainlock"].get("tip_locked_s") is not None))
    is_ok = sum(1 for c in measured if not (c.get("islock_wait") or {}).get("timed_out"))
    lines += ["", "## Locks and recovered signatures", "",
              f"- ChainLock: tip locked and agreed on every node in {cl_ok}/{len(cycles)} cycles",
              f"- InstantSend (after warm-up): every tx locked on every node in {is_ok}/{len(measured)} cycles"]
    dep_ok = dep_n = 0
    for t in SIGN_TYPES:
        signs = signs_of(cycles, t)
        if not signs:
            continue
        control = [(c, s) for c, s in signs if not s.get("dependent")]
        member = [(c, s) for c, s in control if sign_member(c, s, rnode)]
        recovered = [s for _, s in member if any(recovered_on(s, k) for k in s["recovered_after_s"])]
        signed = [s for _, s in member if "rdashd_member" in s]
        dependent = [s for _, s in signs if s.get("dependent")]
        tried = [s for s in dependent if not s.get("skipped")]
        ok = sum(1 for s in tried if any(recovered_on(s, k) for k in s["recovered_after_s"]))
        dep_ok, dep_n = dep_ok + ok, dep_n + len(tried)
        lines.append(f"- {TYPE_NAMES[t]} control: {len(control)} requests, rdashd in the signing quorum for {len(member)}; "
                     f"rdashd's `quorum sign` true {sum(1 for s in signed if s['asked'].get(rnode) is True)}/{len(signed)}; "
                     f"rdashd held {sum(recovered_on(s, rnode) for s in recovered)}/{len(recovered)} of those recovered; "
                     f"differing signatures: {sum(1 for _, s in signs if s.get('distinct_sigs', 1) > 1)}")
        lines.append(f"- {TYPE_NAMES[t]} rdashd-dependent recoveries: {ok}/{len(tried)} "
                     f"({len(dependent) - len(tried)} skipped: no quorum of the type, or rdashd in none tried)")
    lines.append(f"- rdashd-dependent recoveries: {dep_ok}/{dep_n}")
    held = Counter()
    heights = 0
    for c in cycles:
        r = c.get("cl_recsigs") or {}
        heights += r.get("heights", 0)
        held.update(r.get("held", {}))
    if heights:
        lines.append("- ChainLock recovered signatures held, per node (of " + str(heights) + " heights): "
                     + ", ".join(f"node {k}{' (rdashd)' if k == rnode else ''}: {v}" for k, v in sorted(held.items(), key=lambda kv: int(kv[0]))))
    log_lines = [line for c in cycles for line in c.get("rdashd_log", [])]
    sig = significant(log_lines)
    lines += ["", f"## rdashd warning, error and panic lines ({len(log_lines)} in cycles, {len(sig)} not allowlisted; "
              f"{len(outside)} outside the cycles; {len(panics)} stderr panics)", ""]
    lines += [f"- {n}x `{text[:200]}`" for text, n in group_log_lines(log_lines + list(outside) + list(panics))[:40]] or ["None."]
    if BENIGN_LOG:
        lines += ["", "Allowlisted groups:"] + [f"- `{p}`: {why}" for p, why in BENIGN_LOG]
    return "\n".join(lines) + "\n"


def write_summary(out, name="summary.md", rescan=False):
    meta, events = load(out)
    rescanned = None
    if rescan:
        rescanned = rescan_logs(out, meta, [e for e in events if e.get("event") == "cycle"])
    path = Path(out) / name
    path.write_text(summarize(meta, events, rescanned))
    return path


# ---- running -----------------------------------------------------------------------------------

def run(args):
    out = Path(args.out).resolve()
    if out.exists() and any(out.iterdir()):
        print(f"mn-soak: {out} is not empty", file=sys.stderr)
        return 2
    out.mkdir(mode=0o700, parents=True, exist_ok=True)
    out.chmod(0o700)
    rdashd, rdash_cli = Path(args.rdashd).resolve(), Path(args.rdash_cli).resolve()
    for b in (rdashd, rdash_cli):
        if not b.exists():
            print(f"mn-soak: no {b}; build rdashd and rdash-cli with --features rdashd/dkg-runtime", file=sys.stderr)
            return 2
    fc = func_compat()
    tree = fc.build(args.core, args.work, "v24.0", True, fc.G2_CORE_BRANCH, fc.G2_RELEASE)
    functional = tree / "src/test/functional"
    script = NONREC if getattr(args, "cmd", "run") == "nonrec" else SOAK
    for name in {SOAK, script}:
        shutil.copy2(HERE / name, functional / name)
    port_lock = PORT_LOCKS / f"ports-{args.portseed}.lock"
    PORT_LOCKS.mkdir(parents=True, exist_ok=True)
    lock = open(port_lock, "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print(f"mn-soak: another soak holds {port_lock}", file=sys.stderr)
        return LOCK_BUSY
    env = {**os.environ, "RDASHD": str(rdashd), "RDASH_CLI": str(rdash_cli), "RDASHD_NODES": f"mn:{args.mn}",
           "RDASHD_OBSERVER": "0", "PYTHONPATH": str(fc.pylib(args.work)), "TEST_RUNNER_PORT_MIN": str(PORT_MIN),
           "RDASHD_TEST_TIMEOUT": str(args.seconds + 3600)}
    cmd = [sys.executable, str(functional / script), f"--tmpdir={out / 'nodes'}", "--nocleanup",
           f"--portseed={args.portseed}", f"--soak-out={out}",
           f"--soak-seconds={args.seconds}", f"--soak-regular={args.regular}", f"--soak-evo={args.evo}",
           f"--soak-txs={args.txs}", f"--soak-signs={args.signs}", f"--soak-max-cycles={args.max_cycles}"]
    if script == NONREC:
        cmd += [f"--nonrec-requests={args.requests}", f"--nonrec-max-cycles={args.cycles}"]
        cmd += ["--nonrec-skip-b"] if args.skip_b else []
        cmd += ["--nonrec-rdashd-send"] if args.rdashd_send else []
    meta = {"rdc_head": subprocess.run(["git", "-C", str(RDC), "rev-parse", "HEAD"], capture_output=True, text=True).stdout.strip(),
            "rdashd": str(rdashd), "core_tree": json.loads((tree / "tree.json").read_text()), "cmd": cmd,
            "mask": env["RDASHD_NODES"]}
    (out / "run.json").write_text(json.dumps(meta, indent=1, sort_keys=True) + "\n")
    with open(out / "test.log", "w") as log:
        status = subprocess.run(cmd, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
    if script == NONREC:
        print((out / "nonrec.json").read_text() if (out / "nonrec.json").exists() else "mn-soak: no nonrec.json")
    elif (out / "cycles.jsonl").exists():
        print(write_summary(out))
    print(f"mn-soak: soak exit {status}; {out}")
    return status


def selftest():
    import unittest
    suite = unittest.defaultTestLoader.discover(str(HERE), pattern="test_mn_soak.py")
    return 0 if unittest.TextTestRunner(verbosity=1).run(suite).wasSuccessful() else 1


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    n = sub.add_parser("nonrec")
    for q in (r, n):
        q.add_argument("--out", required=True)
        q.add_argument("--mn", type=int, default=2, help="the regular masternode ordinal on rdashd (G2's default)")
        q.add_argument("--regular", type=int, default=4)
        q.add_argument("--evo", type=int, default=3)
        q.add_argument("--txs", type=int, default=3)
        q.add_argument("--portseed", type=int, default=PORT_SEED, help="the test framework's port seed; one soak per seed")
        q.add_argument("--rdashd", default=RDC / "target/debug/rdashd")
        q.add_argument("--rdash-cli", default=RDC / "target/debug/rdash-cli")
        q.add_argument("--core", default=Path.home() / "workspace/dash")
        q.add_argument("--work", default=Path(os.environ.get("TMPDIR", "/work/tmp")) / "func-compat")
    r.add_argument("--seconds", type=int, default=3600)
    r.add_argument("--signs", type=int, default=2)
    r.add_argument("--max-cycles", type=int, default=0)
    n.add_argument("--requests", type=int, default=12, help="non-recovery requests (case A)")
    n.add_argument("--cycles", type=int, default=10, help="DKG cycles before case A gives up")
    n.add_argument("--skip-b", action="store_true", help="skip case B")
    n.add_argument("--rdashd-send", action="store_true",
                   help="require rdashd's llmq-sigs 'sending to' line per non-recovery request (an rdashd with those lines)")
    n.set_defaults(seconds=7200, signs=0, max_cycles=0)
    s = sub.add_parser("summary")
    s.add_argument("out")
    s.add_argument("--output", default="summary.md", help="file name under OUT")
    s.add_argument("--rescan", action="store_true", help="re-read rdashd's log lines from its debug.log and stderr")
    sub.add_parser("selftest")
    args = p.parse_args(argv)
    if args.cmd in ("run", "nonrec"):
        return run(args)
    if args.cmd == "summary":
        print(write_summary(args.out, args.output, args.rescan))
        return 0
    return selftest()


if __name__ == "__main__":
    sys.exit(main())
