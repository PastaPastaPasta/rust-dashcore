#!/usr/bin/env python3
"""MN0-2a capture: no-masternode regtest chains mined by dashd with `generateblock`.

Each configuration starts a fresh regtest dashd with the given chain-parameter arguments, mines
`tip` blocks to one address with `generateblock <address> []` (an empty transaction list, so each
block is exactly CreateNewBlock's on an empty mempool), one block per mock-time step, and records
every header plus the raw blocks from the first DIP0003 height on. No masternode is ever
registered, so the chains hold only null quorum commitments and no ChainLock.

Usage: capture_mn0_evo.py <dashd> <work-dir> <out-json>
"""
import base64
import datetime
import hashlib
import http.client
import json
import os
import socket
import subprocess
import sys
import time

ADDRESS = "ycwedq2f3sz2Yf9JqZsBCQPxp18WU3Hp4J"
MOCK_START = 1700000000
MOCK_STEP = 150
NO_TIMEOUT = 9223372036854775807

CONFIGS = [
    {
        "name": "default",
        "args": [],
        "tip": 500,
    },
    {
        # CbTx v1 (DIP0003 alone), v2 (DIP0008), v3 in the CreditPool era (V20), v3 with MN_RR.
        "name": "moved",
        "args": [
            "-dip3params=2:2",
            "-testactivationheight=dip0008@50",
            "-testactivationheight=v20@150",
            "-testactivationheight=mn_rr@200",
        ],
        "tip": 270,
    },
    {
        # V24 by signalling (window 100, no EHF): active from 300, before V20 at 432, so the CbTx
        # goes from none to v4.
        "name": "v24",
        "args": [f"-vbparams=v24:0:{NO_TIMEOUT}:0:100:80:60:5:0"],
        "tip": 480,
    },
    {
        # MN_RR (432) below BRR: the platform reward follows the masternode payment's increase
        # steps (350 + k * 10), the ninth above 440.
        "name": "brr-late",
        "args": ["-testactivationheight=brr@1000"],
        "tip": 460,
    },
    {
        # MN_RR below the reallocation start (20 with BRR at 1): the platform reward is a share of
        # 20 % of the block value to 19, of 75 % from 20.
        "name": "mnrr-early",
        "args": [
            "-dip3params=2:2",
            "-testactivationheight=v20@10",
            "-testactivationheight=mn_rr@10",
        ],
        "tip": 60,
    },
    {
        # V19 above DIP0003: legacy-scheme null commitments (versions 1 and 2) to 99, then 3 and 4.
        "name": "legacy-bls",
        "args": [
            "-dip3params=2:2",
            "-testactivationheight=v19@100",
            "-testactivationheight=v20@150",
            "-testactivationheight=mn_rr@150",
        ],
        "tip": 200,
    },
    {
        # V24 active from 300 with V20 off: the CbTx stays v2 until V20 at 350, then v4.
        "name": "v24-before-v20",
        "args": [
            "-dip3params=2:2",
            "-testactivationheight=v20@350",
            "-testactivationheight=mn_rr@350",
            f"-vbparams=v24:0:{NO_TIMEOUT}:0:100:80:60:5:0",
        ],
        "tip": 370,
    },
]


def free_port():
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class Node:
    def __init__(self, dashd, datadir, args):
        os.makedirs(datadir, mode=0o700)
        self.rpc = free_port()
        self.auth = base64.b64encode(b"u:p").decode()
        self.proc = subprocess.Popen(
            [dashd, "-regtest", f"-datadir={datadir}", f"-rpcport={self.rpc}", "-listen=0", "-server=1",
             "-rpcuser=u", "-rpcpassword=p", f"-mocktime={MOCK_START}", "-printtoconsole=0", *args],
            stdout=subprocess.DEVNULL)
        for _ in range(600):
            try:
                self.call("getblockcount")
                return
            except (OSError, RuntimeError):
                if self.proc.poll() is not None:
                    raise RuntimeError(f"dashd exited {self.proc.returncode}")
                time.sleep(0.2)
        raise RuntimeError("dashd did not answer")

    def call(self, method, *params):
        c = http.client.HTTPConnection("127.0.0.1", self.rpc, timeout=600)
        c.request("POST", "/", json.dumps({"method": method, "params": list(params), "id": 1}),
                  {"Authorization": "Basic " + self.auth})
        reply = json.loads(c.getresponse().read())
        if reply.get("error"):
            raise RuntimeError(reply["error"])
        return reply["result"]

    def stop(self):
        try:
            self.call("stop")
        except (OSError, RuntimeError):
            pass
        self.proc.wait(120)


def capture(dashd, work, config):
    node = Node(dashd, os.path.join(work, config["name"]), config["args"])
    try:
        for h in range(1, config["tip"] + 1):
            node.call("setmocktime", MOCK_START + h * MOCK_STEP)
            node.call("generateblock", ADDRESS, [])
        hashes = [node.call("getblockhash", h) for h in range(config["tip"] + 1)]
        headers = [node.call("getblockheader", h, False) for h in hashes]
        dip3 = next(h for h, bh in enumerate(hashes)
                    if node.call("getblock", bh, 2)["tx"][0].get("type") == 5)
        blocks = [node.call("getblock", bh, 0) for bh in hashes[dip3:]]
        return {
            "name": config["name"],
            "args": config["args"],
            "tip": config["tip"],
            "dip3_height": dip3,
            "headers": headers,
            "blocks": blocks,
            "deployments_at_tip": node.call("getdeploymentinfo")["deployments"],
            "script_pubkey": node.call("validateaddress", ADDRESS)["scriptPubKey"],
            "subversion": node.call("getnetworkinfo")["subversion"],
        }
    finally:
        node.stop()


def main():
    dashd, work, out = sys.argv[1:4]
    os.makedirs(work, mode=0o700, exist_ok=True)
    with open(dashd, "rb") as f:
        sha = hashlib.sha256(f.read()).hexdigest()
    chains = [capture(dashd, work, c) for c in CONFIGS]
    fixture = {
        "provenance": {
            "tool": "contrib/node/mn0-captures/capture_mn0_evo.py",
            "dashd_subversion": chains[0]["subversion"],
            "dashd_sha256": sha,
            "captured_utc": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "address": ADDRESS,
            "mocktime": f"{MOCK_START} + height * {MOCK_STEP}",
            "rpc": "generateblock <address> [] per block",
        },
        "chains": [{k: v for k, v in c.items() if k != "subversion"} for c in chains],
    }
    with open(out, "w") as f:
        json.dump(fixture, f, indent=1, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
