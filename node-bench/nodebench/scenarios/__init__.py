"""Scenario registry.

A scenario is a function `run(ctx)` that drives one implementation and records
metrics on `ctx.result`. `ctx` carries the parsed CLI arguments, the
implementation under test, CPU slices and an artifact directory.
"""

from dataclasses import dataclass, field

REGISTRY = {}
MODULES = ["ibd", "reindex", "startup", "mempool", "rpc", "spv", "microbench", "masternode"]


@dataclass
class Scenario:
    name: str
    group: str
    summary: str
    run: object
    metrics: dict = field(default_factory=dict)  # name -> definition (for `run list -v` and docs)
    defaults: dict = field(default_factory=dict)  # default CLI parameter values
    pending: str = None  # reason, for methods defined but not runnable yet
    long: bool = False  # hours; excluded from `run all`
    # Gate profile: params -> the gated metric names every comparison of this scenario must observe
    # (compare.required_metrics, which also adds the defaults' inventory). None/empty: informational
    # scenario; compare.inventory_row then keeps it from ever yielding a PASS.
    required: object = None
    accepts: tuple = ()  # run parameters it uses beyond `defaults` (others are refused, cli.check_params)


def scenario(name, group, summary, metrics=None, defaults=None, pending=None, long=False, required=None,
             accepts=()):
    def wrap(fn):
        REGISTRY[name] = Scenario(name, group, summary, fn, metrics or {}, defaults or {}, pending, long, required,
                                  tuple(accepts))
        return fn

    return wrap


def load_all():
    import importlib

    for mod in MODULES:
        importlib.import_module(f"{__name__}.{mod}")

    return REGISTRY
