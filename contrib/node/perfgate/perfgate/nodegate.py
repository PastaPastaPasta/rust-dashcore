"""Node rows on the PF0 chain (PF0-N plan rev4.1 §1, §3, §7, D46, D58, D75, D91): how a gate measures them on any
revision, PR heads included.

A `Session` is one gate's node path: it stops this root's stale units (D13), checks the tmpfs worst case (§6) and the
broker socket path's length, runs the self-probe (D58) and starts the holder with every side's exe in `GATE`. Each
row then gets, per side, one preflight invocation (§7: node-bench's record says ok, refused or error; ok without a
measured run in node-bench is preflight-only, D92) and, for the sides whose preflight is ok, `repeat` measured
invocations, interleaved across the sides, each on a fresh RUN (D46). The counts come from the broker's per-spawn
files in res, never from anything node-bench or the node wrote.
"""

import json
import os
import shutil

from . import invocation, node, noderow, paths

HOLDER_SLACK_S = 600
SUN_PATH_MAX = 107  # sockaddr_un.sun_path, less its NUL
ALLOC_SAMPLES = ("allocs", "alloc_bytes", "read_bytes", "write_bytes", "read_syscalls", "write_syscalls",
                 "peak_rss_kib")


class RunError(Exception):
    pass


def exe_key(label, binary):
    return f"{label}-{binary}"


def alloc_samples(records):
    """The preload counter's records of one spawn, summed over its processes (peak RSS: the largest)."""
    out = {}
    for d in records:
        for k in ALLOC_SAMPLES:
            v = d.get(k, 0)
            out[k] = max(out.get(k, 0), v) if k == "peak_rss_kib" else out.get(k, 0) + v
    return out


class Session:
    """One gate's holder and invocations. `exes` is {key: exe path}; `runs` bounds the measured invocations per
    row and side, escalation included, for the holder's RuntimeMaxSec."""

    def __init__(self, exes, wls, runs, log, test=False):
        self.log, self.wls = log, wls
        self.prefix = invocation.unit_prefix(test=test)
        self.tl = invocation.tools()
        # short: the broker's socket is <dir>/inv/<key>-<n>/mnt/ctl/broker, and AF_UNIX paths stop at 107 bytes
        self.dir = os.path.join(paths.root(), "node", f"g-{os.getpid()}")
        self.n, self.holder = 0, None
        self.nb = noderow.node_bench_dir()
        self.contract = noderow.contract(self.nb)
        units, removed = invocation.gc(self.prefix)
        if units or removed:
            log(f"node path: stopped {len(units)} stale units, removed {len(removed)} stale gate dirs")
        os.makedirs(self.dir, 0o700)
        try:
            # the longest broker socket: <dir>/inv/<key>-<n>/mnt/ctl/broker, n up to every invocation of the gate
            n = len(str(len(exes) * len(wls) * (1 + runs)))
            longest = len(os.fsencode(os.path.join(self.dir, "inv", f"{max(exes, key=len, default='')}-{'9' * n}",
                                                   "mnt", "ctl", "broker")))
            if longest > SUN_PATH_MAX:
                raise invocation.ChainError(f"the root {paths.root()} is too long for the broker's socket path "
                                            f"({longest} > {SUN_PATH_MAX} bytes): use a shorter PERFGATE_ROOT")
            invocation.tmpfs_total_check([(w.name, noderow.limits(w), noderow.prep(w) is not None) for w in wls],
                                         gate=invocation.gate_size(exes.values()))
            probe = invocation.self_probe(self.dir, self.prefix, self.tl)
            log(f"node path: self-probe ok ({probe['instructions']:,} instructions on CPUs {probe['cpus']})")
            per_side = sum(self.bound(w, "preflight") + runs * self.bound(w, "run") for w in wls)
            # assigned before start(), so close() stops a holder whose start failed part way
            self.holder = invocation.Holder(self.dir, f"{self.prefix}holder", exes,
                                            len(exes) * per_side + HOLDER_SLACK_S, self.tl)
            self.holder.start()
        except BaseException:
            self.close()
            raise

    def bound(self, w, phase):
        budget = noderow.phases(w, self.nb, self.contract)[phase][0]
        return invocation.spawn_bound(budget, noderow.limits(w)) + 30

    def close(self):
        if self.holder is not None:
            self.holder.stop()
            self.holder = None
        invocation.gc(self.prefix)  # this gate's invocation scopes too (gates on one root never overlap, D13)
        shutil.rmtree(self.dir, ignore_errors=True)

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()

    def invoke(self, key, w, phase):
        """One invocation of row `w`'s `phase` on exe `key`; its Result."""
        self.holder.check()
        self.n += 1
        budget, cmd, binds = noderow.phases(w, self.nb, self.contract)[phase]
        inv = invocation.Invocation(self.holder, key, self.n, f"{self.prefix}{key}-{self.n}",
                                    limits=noderow.limits(w), budget=budget, view_cmd=cmd, ro_binds=binds)
        return inv.run()

    def preflight(self, key, w):
        """The row's preflight on exe `key`: {status: ok|refused|preflight-only, preflight: record (with `key`),
        argv} or {status: error, error}. preflight-only: the node accepted the argv, but node-bench has no measured
        run for the row."""
        r = self.invoke(key, w, "preflight")
        man = r.json("manifest.json")
        status = man.get("status") if isinstance(man, dict) else None
        errs = r.errors()
        if status == "error":  # node-bench's own verdict (it exits 1 for it): the error it names comes first
            errs = [str(man.get("error"))] + [e for e in errs if not e.startswith("node-bench ended")]
        elif status not in ("ok", "refused"):
            errs.append(f"no preflight manifest ({r.outer and r.outer.get('manifest')})")
        elif not isinstance(man.get("measured"), bool):
            errs.append("the preflight manifest does not say whether the row has a measured run")
        if errs:
            return {"status": "error", "error": "preflight: " + "; ".join(errs)[:1500]}
        rec = dict(man["preflight"], key=w.key())
        if status == "ok" and not man["measured"]:  # accepted, but no PF0 measured path yet (D86, D92)
            status = "preflight-only"
        return {"status": status, "preflight": rec, "argv": man.get("preflight_argv")}

    def run(self, key, w, argv_sha256):
        """One measured invocation: {counter: value} of its one run spawn, or RunError. `argv_sha256` is the
        preflight's: the run must start the node with the argv the preflight accepted (§7)."""
        r = self.invoke(key, w, "run")
        errs = r.errors()
        man = r.json("manifest.json")
        if errs or not isinstance(man, dict) or man.get("status") != "ok":
            why = "; ".join(errs) or (man.get("error") if isinstance(man, dict) else None) or "no manifest"
            raise RunError(f"{w.name}: {str(why)[:1500]}")
        if man.get("argv_sha256") != argv_sha256:
            raise RunError(f"{w.name}: the measured run's argv ({man.get('argv_sha256')}) is not the one its "
                           f"preflight accepted ({argv_sha256})")
        runs = [r.json(n) for n in sorted(os.listdir(r.res)) if n.startswith("spawn-") and n.endswith(".result.json")]
        runs = [s for s in runs if s and s.get("class") == "run"]
        if len(runs) != 1 or runs[0].get("status") != {"exit": 0} or not runs[0].get("measured"):
            raise RunError(f"{w.name}: expected one measured run spawn with exit 0, got "
                           f"{[(s.get('status'), s.get('measured')) for s in runs]}")
        k = runs[0]["spawn"]
        try:
            with open(os.path.join(r.res, f"spawn-{k}.perf")) as f:
                perf = node.parse_perf_csv(f.read())
            with open(os.path.join(r.res, f"spawn-{k}.alloc.json")) as f:
                records = json.load(f)
        except (OSError, ValueError) as e:
            raise RunError(f"{w.name}: the run's counts: {e}") from None
        if runs[0].get("alloc_errors") or not records:
            raise RunError(f"{w.name}: the allocation counter: {runs[0].get('alloc_errors') or 'no record'}")
        missing = [e for e in (x.removesuffix(":u") for x in node.PERF_EVENTS) if e not in perf]
        if missing:
            raise RunError(f"{w.name}: perf did not report {', '.join(missing)}")
        # Plan §10's trusted memory, kept with the counts (INFO, never gated): the helper's wait4 maxrss of its
        # child (the largest of perf, bwrap, the node and what they reaped) and spawn-k's memory.peak (the whole
        # tree, tmpfs and charged page cache included).
        trusted = {"maxrss_kib": (runs[0].get("rusage") or {}).get("maxrss_kib"),
                   "memory_peak_kib": (runs[0].get("memory_peak") or 0) // 1024}
        trusted = {k: v for k, v in trusted.items() if isinstance(v, int) and v > 0}
        return {"instructions": perf["instructions"], "page_faults": perf["page-faults"], **alloc_samples(records),
                **trusted}

    def measure(self, keys, w, repeat):
        """Row `w` on every exe of `keys`: {key: result}, the measured runs interleaved across the keys."""
        out = {k: self.preflight(k, w) for k in keys}
        for k, res in out.items():
            self.log(f"  {w.name} ({k}): preflight {res['status']}"
                     + (f" on {[x['option'] for x in res['preflight']['refused_on']]}" if res["status"] == "refused"
                        else f": {res['error']}" if res["status"] == "error" else ""))
        live = [k for k in keys if out[k]["status"] == "ok"]
        samples = self.repeat(live, w, repeat, out)
        for k in live:
            if out[k]["status"] == "ok":
                out[k].update(iters=1, samples=samples[k])
        return out

    def repeat(self, keys, w, repeat, out):
        """`repeat` interleaved runs per key; a failed key's result becomes the error."""
        samples = {k: {} for k in keys}
        for i in range(repeat):
            for k in keys:
                if out[k]["status"] != "ok":
                    continue
                self.log(f"  {w.name} ({k}): run {i + 1}/{repeat}")
                try:
                    for c, v in self.run(k, w, out[k]["preflight"]["argv_sha256"]).items():
                        samples[k].setdefault(c, []).append(v)
                except (RunError, invocation.ChainError) as e:
                    out[k] = {"status": "error", "error": str(e), "preflight": out[k].get("preflight")}
        return samples
