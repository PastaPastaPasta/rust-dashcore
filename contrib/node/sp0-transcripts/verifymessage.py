#!/usr/bin/env python3
"""Core's verdicts on spork message-form signatures: regtest dashd v24 `signmessagewithprivkey`
and `verifymessage` (both `CMessageSigner`'s, as `CSporkMessage` uses on every network but
testnet).

For each spork (ID, value, time) below, dashd signs the spork's message (`ID‖value‖time` in
decimal) with Core's functional-test spork key, then judges altered signatures against regtest's
spork address: every header byte 0..255, s negated with the recovery ID's parity flipped (high-S)
and without the flip, and random bit flips. Each output line is
`ID VALUE TIME SIGNATURE_HEX VERDICT`, VERDICT being `signed` for dashd's own signature, else
`true` or `false`.

Usage:
  P2A_DASHD=/path/to/dashd-24.0.0-rc.3/bin/dashd verifymessage.py OUT_FILE
"""

import base64
import importlib.util
import json
import os
import pathlib
import random
import sys
import urllib.request

os.environ.setdefault("P2A_CAPTURE_DIR", "/work/scratch/sp0/capture")
os.environ.setdefault("P2A_PORT_BASE", "31900")
_p2a = pathlib.Path(__file__).resolve().parent.parent / "p2a-transcripts" / "capture.py"
_spec = importlib.util.spec_from_file_location("p2a_capture", _p2a)
p2a = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(p2a)

SPORK_KEY = "cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK"
SPORK_ADDR = "yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW"
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
SPORKS = [
    (10001, 0, 1700000000),
    (10016, 4070908800, 1700000000),
    (10018, 0, 1),
    (10020, 1, 1556000000),
    (10022, -1, 1700007200),
    (-5, -9223372036854775808, 9223372036854775807),
    (2147483647, 9223372036854775807, -9223372036854775808),
    (12345, 42, 0),
]
FLIPS = 64


def batch(node, calls):
    """One JSON-RPC batch; the results, errors as None."""
    body = json.dumps([{"jsonrpc": "1.0", "id": i, "method": m, "params": p}
                       for i, (m, p) in enumerate(calls)]).encode()
    req = urllib.request.Request(
        f"http://127.0.0.1:{node.rpc_port}/", data=body,
        headers={"Authorization": "Basic " + base64.b64encode(b"p2a:p2a").decode()})
    with urllib.request.urlopen(req, timeout=600) as r:
        replies = sorted(json.load(r), key=lambda x: x["id"])
    return [None if x.get("error") else x["result"] for x in replies]


def variants(sig, rng):
    out = [bytes([h]) + sig[1:] for h in range(256)]
    s = int.from_bytes(sig[33:65], "big")
    neg = (N - s).to_bytes(32, "big")
    out.append(bytes([27 + ((sig[0] - 27) ^ 1)]) + sig[1:33] + neg)
    out.append(sig[:33] + neg)
    for _ in range(FLIPS):
        b = bytearray(sig)
        i = rng.randrange(1, 65)
        b[i] ^= 1 << rng.randrange(8)
        out.append(bytes(b))
    return out


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    p2a.ROOT.mkdir(parents=True, exist_ok=True, mode=0o700)
    rng = random.Random(0x5B0)
    node = p2a.Node("verifymessage", 6, p2a.SIM_START)
    lines = []
    try:
        for sid, value, time_ in SPORKS:
            msg = f"{sid}{value}{time_}"
            sig = base64.b64decode(node.rpc("signmessagewithprivkey", SPORK_KEY, msg))
            lines.append(f"{sid} {value} {time_} {sig.hex()} signed")
            vs = variants(sig, rng)
            verdicts = batch(node, [("verifymessage", [SPORK_ADDR, base64.b64encode(v).decode(), msg]) for v in vs])
            for v, ok in zip(vs, verdicts):
                assert ok in (True, False), ok
                lines.append(f"{sid} {value} {time_} {v.hex()} {str(ok).lower()}")
    finally:
        node.stop()
    pathlib.Path(sys.argv[1]).write_text("\n".join(lines) + "\n")
    print(f"{sys.argv[1]}: {len(lines)} lines, {sum(l.endswith(' true') for l in lines)} true")


if __name__ == "__main__":
    main()
