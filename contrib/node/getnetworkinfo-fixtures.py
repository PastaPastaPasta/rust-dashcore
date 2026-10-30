#!/usr/bin/env python3
"""Records dashd's raw `getnetworkinfo` replies, the fixtures of `dashd-rpc/src/peers/tests.rs`
(BL-391).

Each configuration starts the pinned dashd (v24.0.0-rc.3) on a fresh regtest datadir with

  -regtest -printtoconsole=0 -disablewallet -rpcuser=u -rpcpassword=p -rpcport=P -port=P+1
  -dnsseed=0 -fixedseeds=0 [-bind=127.0.0.1 unless -listen=0] [-listenonion=0] <extra>

and writes the HTTP body of its first `getnetworkinfo` reply, byte for byte, to
<out>/<name>.raw, after a line with the arguments. The fixtures are the `result` of those bodies.
`networkactive0` also records `setnetworkactive true` and the next `getnetworkinfo`.

  getnetworkinfo-fixtures.py --dashd ~/.rust-dashcore-test/dashcore-24.0.0-rc.3/bin/dashd \\
      --out ./raw [--port 21600]
"""
import argparse
import base64
import json
import os
import shutil
import subprocess
import time
import urllib.request

CONFIGS = {
    "default": [],
    "listen0": ["-listen=0"],
    "onlynet_ipv4": ["-onlynet=ipv4"],
    "onlynet_onion": ["-onlynet=onion", "-onion=127.0.0.1:9", "-listenonion=0"],
    "proxy": ["-proxy=127.0.0.1:9", "-listenonion=0"],
    "proxy_norand": ["-proxy=127.0.0.1:9", "-proxyrandomize=0", "-listenonion=0"],
    "networkactive0": ["-networkactive=0"],
    "connect0": ["-connect=0", "-listen=0"],
    "blocksonly": ["-blocksonly=1"],
}


def call(port, method, params=()):
    body = json.dumps({"id": 1, "method": method, "params": list(params)}).encode()
    auth = {"Authorization": "Basic " + base64.b64encode(b"u:p").decode()}
    req = urllib.request.Request(f"http://127.0.0.1:{port}/", body, auth)
    return urllib.request.urlopen(req, timeout=10).read().decode()


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--port", type=int, default=21600, help="the first RPC port (below 32768)")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    for i, (name, extra) in enumerate(CONFIGS.items()):
        dd = os.path.join(a.out, "dd-" + name)
        shutil.rmtree(dd, ignore_errors=True)
        os.makedirs(dd)
        rpc, p2p = a.port + 2 * i, a.port + 2 * i + 1
        argv = [a.dashd, f"-datadir={dd}", "-regtest", "-printtoconsole=0", "-disablewallet",
                "-rpcuser=u", "-rpcpassword=p", f"-rpcport={rpc}", f"-port={p2p}", "-dnsseed=0",
                "-fixedseeds=0", *extra]
        if "-listen=0" not in extra:
            argv.append("-bind=127.0.0.1")
            if "-listenonion=0" not in extra:
                argv.append("-listenonion=0")
        node = subprocess.Popen(argv, env={"HOME": dd, "PATH": "/usr/bin:/bin"})
        reply = ""
        for _ in range(120):
            try:
                reply = call(rpc, "getnetworkinfo")
                json.loads(reply)["result"]
                break
            except Exception:
                reply = ""
                time.sleep(0.5)
        with open(os.path.join(a.out, name + ".raw"), "w") as f:
            f.write(" ".join(argv[1:]) + "\n" + reply)
        if name == "networkactive0" and reply:
            then = call(rpc, "setnetworkactive", [True]) + call(rpc, "getnetworkinfo")
            with open(os.path.join(a.out, "networkactive0_then_true.raw"), "w") as f:
                f.write(then)
        try:
            call(rpc, "stop")
        except Exception:
            pass
        node.wait(timeout=60)
        shutil.rmtree(dd, ignore_errors=True)
        print(name, "ok" if reply else "FAILED")


if __name__ == "__main__":
    main()
