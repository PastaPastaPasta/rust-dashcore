#!/usr/bin/env python3
"""The chain-parameter command-line args of the last dashd start recorded in a debug.log.

Functional tests pass chain parameters (-vbparams, -dip3params, ...) on the command line, not in
dash.conf; a node restarted without them sees an inconsistent evo state. The start-time
-mocktime is left out: the tests bump mocktime over RPC, so it would predate later blocks.
"""
import sys

KEEP = ("dip3params", "vbparams", "llmqtest", "testactivationheight", "budgetparams", "highsubsidy")


def last_start_args(debug_log):
    starts, current = [], []
    for line in open(debug_log, errors="replace"):
        if "Dash Core version" in line and current:
            starts.append(current)
            current = []
        if "Command-line arg: " in line:
            name, _, value = line.split("Command-line arg: ", 1)[1].strip().partition("=")
            if name.startswith(KEEP):
                current.append(f"-{name}={value.strip(chr(34))}")
    if current:
        starts.append(current)
    return starts[-1] if starts else []


if __name__ == "__main__":
    print("\n".join(last_start_args(sys.argv[1])))
