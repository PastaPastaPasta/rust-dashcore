"""Node workloads: the node chain's compiled parts and the counters' formats (PF0-N plan rev4.1).

A node row runs node-bench in PF0 mode inside the chain (nodegate.py, invocation.py): the broker spawns the node in
a jail under `perf stat` (instructions:u, page faults; inherited by every thread) with the LD_PRELOAD allocation
counter, and writes the counts into res, outside anything node-bench or the node can reach. Counts cover the whole
process, startup and shutdown included. node-bench is the base's tree (D1).
"""

import os

from . import sandbox

_HERE = os.path.dirname(os.path.realpath(__file__))
PRELOAD_SRC = os.path.join(_HERE, "alloc_preload.c")
NODE_LAUNCH_SRC = os.path.join(_HERE, "node_launch.c")  # the node chain's helper; the bench rows keep launch.c (D44)
BROKER = os.path.join(_HERE, "broker.py")
CGSETUP = os.path.join(_HERE, "cgsetup.py")
PERF_EVENTS = ("instructions:u", "page-faults")


PRELOAD_FLAGS = (("-shared", "-fPIC"), ("-ldl",))  # preload_lib()'s compile and link flags (§8 component 7)


def preload_lib():
    """The allocation counter, compiled once per source version (the broker binds it into every jail, so base and
    head use the same library)."""
    return sandbox.compiled(PRELOAD_SRC, "perfgate_alloc", *PRELOAD_FLAGS)


def node_launch(test_hooks=False):
    """node-launch (`--into`, `--jail`, `--client`), compiled once per source version under its own name, so the
    bench launcher's build is untouched (D70). `test_hooks` is the tests' build (D9), never the gate's."""
    if test_hooks:
        return sandbox.compiled(NODE_LAUNCH_SRC, "perfgate_node_launch_test", ("-DPF0_TEST_HOOKS",))
    return sandbox.compiled(NODE_LAUNCH_SRC, "perfgate_node_launch")


def parse_perf_csv(text):
    """{event: count} from `perf stat -x,`, without the `:u` suffix; raises when an event was not counted or was
    multiplexed."""
    out = {}
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        f = line.split(",")
        if len(f) < 3:
            continue
        value, event = f[0], f[2]
        if not value[:1].isdigit():
            raise ValueError(f"perf did not count {event}: {value}")
        if len(f) > 4 and f[4] and float(f[4]) < 100.0:
            raise ValueError(f"perf multiplexed {event} ({f[4]}% running)")
        out[event.removesuffix(":u")] = int(float(value))
    return out
