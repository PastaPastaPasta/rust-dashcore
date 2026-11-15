#!/usr/bin/env python3
"""Capture Core v24.0.0's platform-ban P2P exchange on regtest.

This is a thin capture wrapper around Core's ``p2p_platform_ban.py`` functional
test, run through the LQ3a capture harness.  The test still performs Core's
full Evo-node and platform-quorum setup; this wrapper records every
``platformban`` payload delivered to its P2P observers.
"""

import json

from test_framework import p2p
from test_framework.messages import hash256
from test_framework.p2p import P2PInterface

import p2p_platform_ban as core_test


CAPTURED = []


class CapturingPlatformBanInterface(P2PInterface):
    """Keep the exact wire payload and its Core inventory hash."""

    def on_platformban(self, message):
        payload = message.serialize()
        CAPTURED.append({
            "command": "platformban",
            "inv_hash": hash256(payload[:-96])[::-1].hex(),
            "payload": payload.hex(),
        })
        super().on_platformban(message)


# The Core test looks up this name when it creates each observer.
core_test.PlatformBanInterface = CapturingPlatformBanInterface


class PlatformBanCapture(core_test.PlatformBanMessagesTest):
    def set_test_params(self):
        super().set_test_params()

    def add_options(self, parser):
        super().add_options(parser)
        parser.add_argument("--out", required=True, help="capture JSON path")

    def run_test(self):
        try:
            super().run_test()
            status = "completed"
        except Exception:
            status = "failed"
            raise
        finally:
            with open(self.options.out, "w") as output:
                json.dump({"status": status, "messages": CAPTURED}, output, indent=2, sort_keys=True)
                output.write("\n")


if __name__ == "__main__":
    PlatformBanCapture().main()
