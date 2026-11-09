"""The broker with the test hooks (D9). Tests start this instead of broker.py; production never does, and the
production broker refuses a config with test settings.

    python3 -I -B broker_hooked.py --broker BROKER --test TEST_JSON -- --config CONFIG --report FD
"""

import importlib.util
import json
import os
import sys


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main(argv):
    i = argv.index("--")
    opts = dict(zip(argv[1:i:2], argv[2:i:2], strict=True))
    broker = load("pf0_broker", opts["--broker"])
    hooks = load("pf0_broker_hooks", os.path.join(os.path.dirname(os.path.realpath(__file__)), "broker_hooks.py"))
    with open(opts["--test"]) as f:
        test = json.load(f)
    return broker.main(["broker.py", *argv[i + 1:]], hooks=hooks.Hooks(test))


if __name__ == "__main__":
    sys.exit(main(sys.argv))
