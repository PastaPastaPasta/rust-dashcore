"""The node rows' PF0 plan (PF0-N plan rev4.1 §1, §7, §12, D46, D75, D89): what each invocation of a node row runs,
with which spawn budget, and node-bench's preflight contract, read from node-bench's own source.

A node row is one preflight invocation (the batch over the row's measured argv) and, when its workload has a PF0
measured path, one invocation per measured repetition, each on a fresh RUN (D46). node-bench is the base's tree
(D1), bound read-only into the view at NODE_BENCH_VIEW; it names only the stub, never the PR exe.
"""

import ast
import os

from . import invocation, paths

NODE_BENCH_VIEW = "/pf0/node-bench"
RUN_BUDGET = {"version": 1, "run": 1}  # node-bench's describe() makes the one version spawn
CONTRACT = ("REFUSAL_RE", "PREFLIGHT_VERSION", "MAX_RETRIES", "SETTLE_S", "ARGV_VOLATILE")


def node_bench_dir():
    """The base's node-bench tree: the checkout running this tool, which harness_problems pins to the base; or
    PERFGATE_NODE_BENCH for local dev, which makes a gate non-authoritative and is refused by `anchor` (D2)."""
    return os.environ.get("PERFGATE_NODE_BENCH") or os.path.join(paths.REPO, "node-bench")


def contract(nb_dir):
    """node-bench's preflight contract (§8 component 17): the module-level constants CONTRACT of nodebench/pf0.py,
    by its AST (perfgate never imports node-bench), `REFUSAL_RE` as its pattern."""
    with open(os.path.join(nb_dir, "nodebench", "pf0.py")) as f:
        tree = ast.parse(f.read())
    out = {}
    for stmt in tree.body:
        if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0], ast.Name):
            name, value = stmt.targets[0].id, stmt.value
            if name in CONTRACT:
                out[name] = ast.literal_eval(value.args[0] if isinstance(value, ast.Call) else value)
    missing = [n for n in CONTRACT if n not in out]
    if missing:
        raise ValueError(f"node-bench's preflight contract lacks {', '.join(missing)}")
    return {n: out[n] for n in CONTRACT}


def view_cmd(w, preflight=False):
    """node-bench in the view, in PF0 mode, for workload row `w`."""
    return [invocation.INTERPRETER, "-I", "-B", f"{NODE_BENCH_VIEW}/run", "workload", w.node, "--pf0", "--impl",
            "rust", "--node-slice", "", "--aux-slice", "", *w.args, *(["--preflight"] if preflight else [])]


def limits(w):
    """The row's §6 limits (no per-row overrides in phase 1)."""
    return dict(invocation.LIMITS)


def phases(w, nb_dir, contract_):
    """{phase: (budget, view_cmd, ro_binds)} of row `w`'s invocations."""
    binds = [(nb_dir, NODE_BENCH_VIEW)]
    return {"preflight": ({"preflight": 1 + contract_["MAX_RETRIES"]}, view_cmd(w, True), binds),
            "run": (dict(RUN_BUDGET), view_cmd(w), binds)}


def prep(w):
    """The row's PREP plan: its fixture master and seed. No phase-1 row has one (D86): genesis starts empty, the
    fixture rows are preflight-only."""
    return None
