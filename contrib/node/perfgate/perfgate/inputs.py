"""A node row's anchor inputs (PF0-N plan rev4.1 §8, D16–D19, D28, D29, D45, D48, D49).

`node_inputs(w, sha)` hashes everything other than the PR exe that moves a node row's counts, as 17 named
components. The anchor stores the whole hash and every component's, so a stale anchor names what changed (D48).
Left out: the gate id, per-gate paths, the PR exe, timestamps and the kernel (D18: a host_meta NOTE). Every argv,
spec and config of components 8-11 is rendered by the same builders the chain uses, with placeholder paths and
per-command values (`/<RES>`, `/<UNIT>`, `/<RUNTIME>` ...) and the tool's own directory as `/<TOOL>`, so the hash
can't drift from what runs and doesn't move with the checkout.

Component 5 is derived (D28): `node_sources()` is the import closure of the node path's roots (D49) over perfgate
modules, every package `__init__.py`, and `run`. The static guard (`unhashed_references`, D29 a) independently
lists every perfgate module or TOOL_DIR file the reviewed snapshot's modules name; the tests require it to be
empty, and the derived list to equal the snapshot (D49).
"""

import ast
import functools
import hashlib
import json
import os
import subprocess

from . import build, invocation, measure, node, noderow, paths, sandbox

ROOTS = ("node.py", "measure.py", "build.py", "broker.py", "outer_init.py", "cgsetup.py", "holder.py")
EXTRA_SOURCES = ("__init__.py", "../run")  # the package marker and the harness's entry point (D28)
C_SOURCES = ("node_launch.c", "alloc_preload.c", "launch.c")  # components 6 and 7
COMPONENTS = ("fixture", "seed", "declaration", "node_bench", "sources", "launcher", "preload", "spec", "jail_argv",
              "view_argv", "outer_argv", "environment", "limits", "perf", "tools", "python", "preflight")
P = {k: f"/<{k.upper()}>" for k in ("gate", "res", "mnt", "cfg", "node_launch", "preload", "node_bench", "unit",
                                    "holder_unit", "runtime", "timeout", "userns", "mntns", "cpus", "exe_sha256",
                                    "tool")}


def _sha(data):
    return hashlib.sha256(data if isinstance(data, bytes) else json.dumps(data, sort_keys=True).encode()).hexdigest()


def _pkg(tool_dir):
    return os.path.join(tool_dir, "perfgate")


def _unpath(obj, real, placeholder):
    """`obj` with every string's `real` path prefix (a whole path component) as `placeholder`."""
    if isinstance(obj, dict):
        return {k: _unpath(v, real, placeholder) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [_unpath(v, real, placeholder) for v in obj]
    if isinstance(obj, str) and (obj == real or obj.startswith(real + "/")):
        return placeholder + obj[len(real):]
    return obj


def _imports(path):
    """The perfgate modules a module imports, module-level and function-local: `from . import x`, `from .x import`,
    `import perfgate.x`, `from perfgate import x`."""
    with open(path) as f:
        tree = ast.parse(f.read())
    out = set()
    for n in ast.walk(tree):
        if isinstance(n, ast.ImportFrom):
            if n.level == 1 or (n.level == 0 and (n.module or "").split(".")[0] == "perfgate"):
                mod = (n.module or "").removeprefix("perfgate").lstrip(".") if n.level == 0 else n.module
                out |= {a.name for a in n.names} if not mod else {mod.split(".")[0]}
        elif isinstance(n, ast.Import):
            out |= {a.name.split(".")[1] for a in n.names if a.name.startswith("perfgate.")}
    return out


def node_sources(tool_dir=None):
    """Component 5's files, relative to the package dir (D28, D49)."""
    pkg = _pkg(tool_dir or paths.TOOL_DIR)
    seen, todo = set(), list(ROOTS)
    while todo:
        name = todo.pop()
        if name in seen or not os.path.exists(os.path.join(pkg, name)):
            continue
        seen.add(name)
        todo += [f"{m}.py" for m in _imports(os.path.join(pkg, name))]
    return sorted(seen | set(EXTRA_SOURCES))


def unhashed_references(hashed, tool_dir=None):
    """D29 (a), independent of the derivation: every perfgate module that a module of the `hashed` list (the
    reviewed snapshot) imports, and every string constant there naming a file of the package dir or TOOL_DIR,
    that is neither in `hashed` nor a C source of components 6-7. Empty when the list is complete."""
    tool_dir = tool_dir or paths.TOOL_DIR
    pkg = _pkg(tool_dir)
    files = {n: n for n in os.listdir(pkg)} | {n: f"../{n}" for n in os.listdir(tool_dir)
                                               if os.path.isfile(os.path.join(tool_dir, n))}
    listed = set(hashed) | set(C_SOURCES)
    out = []
    for name in sorted(hashed):
        path = os.path.join(pkg, name)
        if not name.endswith(".py"):
            continue
        with open(path) as f:
            tree = ast.parse(f.read())
        refs = {f"{m}.py" for m in _imports(path)}
        refs |= {files[n.value] for n in ast.walk(tree)
                 if isinstance(n, ast.Constant) and isinstance(n.value, str) and n.value in files}
        out += [f"{name} names {r}" for r in sorted(refs - listed) if not r.endswith("__pycache__")]
    return out


def _version(*cmd):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True)
    except OSError:
        return None
    lines = (p.stdout or p.stderr).strip().splitlines()
    return lines[0] if p.returncode == 0 and lines else None


@functools.cache
def tool_versions():
    """Component 15 (D17): the node path's tools. Python is the chain's interpreter, not the harness's; strace is
    not on the node path (D39)."""
    return {"bwrap": _version("bwrap", "--version"), "perf": _version("perf", "--version"),
            "systemd": _version("systemctl", "--version"),
            "python": _version(invocation.INTERPRETER, "-c", "import sys; print(sys.version)"),
            "util-linux": _version("unshare", "--version"), "cc": _version("cc", "--version")}


def render(w, nb_dir, contract):
    """Components 8-12 of row `w` (spec, jail_argv, view_argv, outer_argv, environment): the chain's specs, argv
    and configs from its own builders, with placeholders."""
    tl = invocation.tools()
    lim = noderow.limits(w)
    view, outer, broker = {}, {}, {}
    for phase, (budget, cmd, binds) in sorted(noderow.phases(w, nb_dir, contract).items()):
        binds = [(P["node_bench"] if src == nb_dir else src, dst) for src, dst in binds]
        b, o = invocation.configs(tl, f"{P['gate']}/exe/<SIDE>", P["exe_sha256"], P["res"], P["mnt"], P["cfg"],
                                  P["node_launch"], P["preload"], P["cpus"], lim, budget, cmd, None, binds)
        view[phase] = o.pop("view")
        outer[phase] = o
        broker[phase] = b
    b = broker["run"]
    from . import broker as brk

    spec = {"jail": brk.jail_args(b, "/<DATADIR>", ["/<ARGV>"]),
            "chain": brk.chain_argv(b, ("/<SOCK0>", "/<SOCK1>"), "/<INFO>", "/<ALLOC>", "/<PERF_FD>"),
            "chain_unmeasured": brk.chain_argv(b, ("/<SOCK0>", "/<SOCK1>"), "/<INFO>", "/<ALLOC>", None)}
    argv = {"invocation": invocation.invocation_argv(tl, P["unit"], lim, P["runtime"],
                                                     invocation.nsenter_argv(tl, P["userns"], P["mntns"]),
                                                     f"{P['cfg']}/outer.json"),
            "holder": invocation.holder_argv(tl, P["holder_unit"], P["timeout"],
                                             invocation.holder_unshare(tl),
                                             f"{P['gate']}/holder.json"),
            "outer": outer, "broker": broker}
    out = {"spec": spec["jail"], "jail_argv": {"measured": spec["chain"], "unmeasured": spec["chain_unmeasured"]},
           "view_argv": view, "outer_argv": argv,
           "environment": {"view": invocation.VIEW_ENV, "jail": invocation.JAIL_ENV}}
    return _unpath(out, paths.TOOL_DIR, P["tool"])  # the tool's scripts by their place in it (component 5 hashes them)


def components(w, sha, tool_dir=None, nb_dir=None, tools=None):
    """{component: value} of row `w` at revision `sha` (the base: its tool tree runs, D1)."""
    tool_dir = tool_dir or paths.TOOL_DIR
    pkg = _pkg(tool_dir)
    nb_dir = nb_dir or noderow.node_bench_dir()
    contract = noderow.contract(nb_dir)
    lim = noderow.limits(w)

    def file_sha(name):
        with open(os.path.join(pkg, name), "rb") as f:
            return _sha(f.read())

    cc = (tools or tool_versions())["cc"]
    prep = noderow.prep(w)
    r = render(w, nb_dir, contract)
    return {
        "fixture": prep and prep.get("master"),
        "seed": prep and prep.get("seed"),
        "declaration": w.key(),
        "node_bench": build.git("rev-parse", f"{sha}:{build.NODE_BENCH_REL}"),
        "sources": {name: file_sha(name) for name in node_sources(tool_dir)},
        "launcher": {"node_launch.c": file_sha("node_launch.c"), "launch.c": file_sha("launch.c"),
                     "cc_flags": list(sandbox.CC_FLAGS), "cc": cc},
        "preload": {"alloc_preload.c": file_sha("alloc_preload.c"), "flags": node.PRELOAD_FLAGS,
                    "cc_flags": list(sandbox.CC_FLAGS), "cc": cc},
        **r,
        "limits": {"limits": lim, "budgets": {p: b for p, (b, _, _) in noderow.phases(w, nb_dir, contract).items()},
                   "cpu_list": invocation.numa0_cpus(lim["cpus"]), "numa_node": 0},
        "perf": {"perf_counter": list(measure.PERF_COUNTER), "node_events": list(node.PERF_EVENTS)},
        "tools": tools or tool_versions(),
        "python": {"interpreter": invocation.INTERPRETER, "realpath": os.path.realpath(invocation.INTERPRETER),
                   "flags": ["-I", "-B"]},
        "preflight": contract,
    }


def node_inputs(w, sha, **kw):
    """{"inputs": the row's hash, "components": {component: hash}, "tools": component 15's versions} (§8, D48)."""
    comps = components(w, sha, **kw)
    hashes = {k: _sha(comps[k]) for k in COMPONENTS}
    return {"inputs": _sha(hashes), "components": hashes, "tools": comps["tools"]}


def stale_components(anchored, current):
    """The components that changed since the anchor, as text: `tools` names each version change (D48)."""
    out = []
    for k in COMPONENTS:
        if (anchored.get("components") or {}).get(k) == current["components"][k]:
            continue
        if k == "tools":
            old = anchored.get("tools") or {}
            moved = [f"{t} {old.get(t)} → {v}" for t, v in sorted(current["tools"].items()) if old.get(t) != v]
            out.append(f"tools ({'; '.join(moved)})" if moved else "tools")
        else:
            out.append(k)
    return out
