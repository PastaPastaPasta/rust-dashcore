"""A harness for one chain, as its own process, so the kill matrix can kill the harness too (D52).

    python3 -I -B chain_driver.py SPEC_JSON

SPEC: {"root", "dir", "prefix", "cmd", "budget", "limits", "hooks", "node_launch", "perf"}. The driver starts the
holder and the invocation, prints one JSON line with the units and the res dir, waits for the chain, then prints
its result (rc, errors, and whether the holder outlived it).
"""

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.realpath(__file__))))

from perfgate import invocation as iv  # noqa: E402
from perfgate import node  # noqa: E402

HERE = os.path.dirname(os.path.realpath(__file__))


def main(spec):
    tl = iv.tools()
    holder = iv.Holder(spec["dir"], f"{spec['prefix']}holder", {"head": os.path.join(HERE, "fake_rdashd.py")}, 900,
                       tl).start()
    kw = {"broker_extra": {}, "outer_extra": {}}
    if spec.get("perf"):
        kw["broker_extra"]["perf"] = spec["perf"]
        kw["outer_extra"]["exes"] = {"node_launch": os.path.realpath(spec["node_launch"]), "perf": spec["perf"],
                                     "bwrap": tl["bwrap"], "interpreter": os.path.realpath(iv.INTERPRETER)}
    if spec.get("hooks") is not None:
        tj = os.path.join(spec["dir"], "test.json")
        with open(tj, "w") as f:
            json.dump(spec["hooks"], f)
        bcfg = os.path.join(spec["dir"], "inv", "head-0", "cfg", "broker.json")
        kw["outer_extra"]["broker"] = [iv.INTERPRETER, "-I", "-B", os.path.join(HERE, "broker_hooked.py"),
                                       "--broker", node.BROKER, "--test", tj, "--", "--config", bcfg,
                                       "--report", "{report}"]
    inv = iv.Invocation(holder, "head", 0, f"{spec['prefix']}inv", limits=spec["limits"], budget=spec["budget"],
                        view_cmd=spec["cmd"], node_launch=spec["node_launch"], **kw)
    print(json.dumps({"res": inv.res, "unit": inv.unit, "holder_unit": holder.unit, "holder_pid": holder.proc.pid}),
          flush=True)
    r = inv.run()
    alive = holder.alive()
    holder.stop()
    print(json.dumps({"rc": r.rc, "timed_out": r.timed_out, "errors": r.errors(), "holder_alive": alive}),
          flush=True)


if __name__ == "__main__":
    main(json.loads(sys.argv[1]))
