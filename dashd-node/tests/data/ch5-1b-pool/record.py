#!/usr/bin/env python3
"""CH5-1b (BL-581) Legs B and C: dashd's pool and template answers at the observation tips.

Writes `legB.json` and `legC.json` beside this file: for each tip, dashd's answer before any
walk and after each `verifychain` the test's walks make, which must all be equal (the recorder
stops otherwise). An answer keeps only what the comparison reads (plan §4, kinds 2 and 3):

- `quorum dkgstatus`'s `minableCommitments[]`, each with the twelve commitment fields;
- `getblocktemplate`'s `previousblockhash`, `height`, its type-6 transactions' bytes in order,
  and its `coinbase_payload` decoded to `version`, `height`, `merkleRootMNList`,
  `merkleRootQuorums` and, from version 3, `creditPoolBalance` (hashes as serialized, not
  reversed). `bestCLHeightDiff` and `bestCLSignature` are dropped: the node's best ChainLock is
  runtime state the walk does not own.

Each point keeps the node's history to it: starts, stops, submits, `setmocktime` and walks.

Usage, from the repository root, with dashd v24.0.0 from `contrib/setup-dashd.py`:
    python3 dashd-node/tests/data/ch5-1b-pool/record.py <dashd>
"""
import base64
import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time

DASHD = os.path.abspath(sys.argv[1])
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "../../../.."))
FIELDS = ["version", "llmqType", "quorumHash", "quorumIndex", "signersCount", "signers",
          "validMembersCount", "validMembers", "quorumPublicKey", "quorumVvecHash", "quorumSig",
          "membersSig"]


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class Node:
    def __init__(self, work, args):
        self.dir = os.path.join(work, "node")
        os.makedirs(self.dir, mode=0o700)
        self.args = list(args)
        self.proc = None
        self.history = []

    def start(self):
        self.port = free_port()
        argv = [DASHD, f"-datadir={self.dir}", "-regtest", "-listen=0", "-printtoconsole=0",
                f"-rpcport={self.port}", "-disablewallet", "-dnsseed=0", "-fixedseeds=0",
                *self.args]
        self.history.append(["start"])
        self.proc = subprocess.Popen(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                     env={**os.environ, "HOME": self.dir})
        for _ in range(600):
            try:
                self.call("getblockcount")
                return
            except Exception:
                assert self.proc.poll() is None, "dashd exited"
                time.sleep(0.2)
        raise RuntimeError("start timeout")

    def call(self, method, *params):
        cookie = open(os.path.join(self.dir, "regtest", ".cookie")).read().strip()
        c = http.client.HTTPConnection("127.0.0.1", self.port, timeout=600)
        body = json.dumps({"jsonrpc": "1.0", "id": 1, "method": method, "params": list(params)})
        auth = "Basic " + base64.b64encode(cookie.encode()).decode()
        c.request("POST", "/", body, {"Authorization": auth})
        r = json.loads(c.getresponse().read())
        if r.get("error"):
            raise RuntimeError(f"{method}{params}: {r['error']}")
        return r["result"]

    def stop(self):
        self.call("stop")
        self.proc.wait(120)
        self.history.append(["stop"])

    def submit(self, blocks):
        for b in blocks:
            assert self.call("submitblock", b) is None
        self.history.append(["submit", self.call("getblockcount")])

    def verifychain(self, level, depth):
        assert self.call("verifychain", level, depth) is True
        self.history.append(["verifychain", level, depth])

    def clock_after_tip(self):
        """A clock a minute past the tip, so the template is served outside IBD."""
        t = self.call("getblock", self.call("getbestblockhash"))["time"] + 60
        self.call("setmocktime", t)
        self.history.append(["setmocktime", t])


def cbtx(payload_hex):
    p = bytes.fromhex(payload_hex)
    out = {"version": int.from_bytes(p[0:2], "little"), "height": int.from_bytes(p[2:6], "little"),
           "merkleRootMNList": p[6:38].hex()}
    if out["version"] >= 2:
        out["merkleRootQuorums"] = p[38:70].hex()
    if out["version"] >= 3:
        diff = p[70]
        assert diff < 253, "a one-byte height difference"
        out["creditPoolBalance"] = int.from_bytes(p[71 + 96:79 + 96], "little", signed=True)
    return out


def answer(node):
    status = node.call("quorum", "dkgstatus")
    minable = [{k: c[k] for k in FIELDS} for c in status["minableCommitments"]]
    t = node.call("getblocktemplate", {"rules": ["segwit"]})
    type6 = [x["data"] for x in t["transactions"] if x["data"][4:8] == "0600"]
    template = {"previousblockhash": t["previousblockhash"], "height": t["height"],
                "type6": type6, "cbtx": cbtx(t["coinbase_payload"])}
    return {"minableCommitments": minable, "template": template}


def observe(node, height, walks):
    """The answer before and after each walk at the tip: equal at every point."""
    tip = node.call("getbestblockhash")
    assert node.call("getblockcount") == height
    first = answer(node)
    points = [{"after": "no walk", "history": [list(h) for h in node.history]}]
    for level, depth in walks:
        node.verifychain(level, depth)
        assert answer(node) == first, f"{height}: after verifychain {level} {depth}"
        points.append({"after": f"verifychain {level} {depth}",
                       "history": [list(h) for h in node.history]})
    return {"height": height, "tip": tip, "points": points, **first}


def r1_blocks(name):
    p = os.path.join(REPO, "dashd-chainstate/tests/data/v19-window/r1", name)
    return [l for l in open(p).read().splitlines() if l and not l.startswith("#")]


def recorded_blocks(test):
    d = os.path.join(REPO, "test-vectors/statedigest/regtest-v24", test, "node0")
    raw = subprocess.run(["zstd", "-dc", os.path.join(d, "blocks.dat.zst")], capture_output=True,
                         check=True).stdout
    args = json.load(open(os.path.join(d, "extract.json")))["chain_args"]
    blocks, i = [], 0
    while i + 8 <= len(raw):
        size = int.from_bytes(raw[i + 4:i + 8], "little")
        blocks.append(raw[i + 8:i + 8 + size].hex())
        i += 8 + size
    return args, blocks[1:]


def provenance(args):
    version = subprocess.run([DASHD, "-version"], capture_output=True, text=True).stdout
    return {"dashd": version.splitlines()[0], "args": args,
            "recorded": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "command": "python3 dashd-node/tests/data/ch5-1b-pool/record.py <dashd>"}


def leg_b(work):
    """R1's M, as the probe ran it: the tips 249 and 253, a clock a minute after M's tip."""
    m = r1_blocks("chain.txt")
    summary = json.load(open(os.path.join(REPO,
                                          "dashd-chainstate/tests/data/v19-window/r1/summary.json")))
    t = int.from_bytes(bytes.fromhex(m[-1][136:144]), "little")
    args = summary["chain_args"] + [f"-mocktime={t + 60}"]
    n = Node(work, args)
    n.start()
    n.submit(m[:249])
    tips = [observe(n, 249, [(3, 0), (4, 0)])]
    n.submit(m[249:253])
    tips.append(observe(n, 253, [(3, 0), (3, 3), (4, 0)]))
    n.stop()
    return {"provenance": provenance(args), "tips": tips}


def leg_c(work):
    """`feature_llmq_chainlocks`' node0 under `-assumevalid=0`: synced to 338 and restarted, as
    Leg C restarts it, then 345 and 352, each with a clock a minute after its tip."""
    args, blocks = recorded_blocks("feature_llmq_chainlocks")
    args = args + ["-assumevalid=0"]
    n = Node(work, args)
    n.start()
    n.submit(blocks[:338])
    n.stop()
    n.start()
    tips = []
    for tip in (345, 352):
        n.submit(blocks[n.call("getblockcount"):tip])
        n.clock_after_tip()
        tips.append(observe(n, tip, [(3, 0), (3, 8), (4, 0)]))
    n.stop()
    return {"provenance": provenance(args), "tips": tips}


for name, leg in (("legB.json", leg_b), ("legC.json", leg_c)):
    work = tempfile.mkdtemp(dir=os.environ.get("TMPDIR"))
    try:
        rec = leg(work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    with open(os.path.join(HERE, name), "w") as f:
        json.dump(rec, f, indent=1)
        f.write("\n")
    print("wrote", name, flush=True)
