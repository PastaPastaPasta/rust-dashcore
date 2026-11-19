#!/usr/bin/env python3
"""RP1a-2c fixtures, step 2: dashd's own `decoderawtransaction` for every transaction step 1
read, under each network's parameters, with and without `-deprecatedrpc=service`.

Core prints a CbTx's `bestCLSignature`, an MnHF signal's `sig` and an asset unlock's `quorumSig`
under its process-wide BLS flag, which is legacy until the tip is past V19. The regtest node mines
one block first (V19 is active from height 1), so it prints them as a synced node does; it also
decodes the testnet rows, whose address prefixes and Platform HRP are regtest's. A mainnet node
cannot leave the legacy flag offline: step 3 drops its rows that carry such a field.

Usage: decode.py DASHD RAW.jsonl OUT.jsonl WORKDIR RPCPORT
Each dashd runs offline (-connect=0 -listen=0, no seeds) on its own data directory under
WORKDIR and its own RPC port, and is stopped over RPC before the next one starts.
"""
import base64
import json
import os
import subprocess
import sys
import time
import urllib.request

NET_FLAG = {"main": [], "testnet": ["-testnet"], "regtest": ["-regtest"]}


def rpc_raw(port, method, *params):
    """The reply's bytes as dashd wrote them, so number text survives."""
    body = json.dumps({"jsonrpc": "1.0", "id": 1, "method": method, "params": list(params)})
    req = urllib.request.Request(f"http://127.0.0.1:{port}/", body.encode())
    req.add_header("Authorization", "Basic " + base64.b64encode(b"fx:fx").decode())
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.read().decode()
    except urllib.error.HTTPError as e:
        return e.read().decode()


def rpc(port, method, *params):
    body = json.dumps({"jsonrpc": "1.0", "id": 1, "method": method, "params": list(params)})
    req = urllib.request.Request(f"http://127.0.0.1:{port}/", body.encode())
    req.add_header("Authorization", "Basic " + base64.b64encode(b"fx:fx").decode())
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        return json.loads(e.read())


def main():
    dashd, raw_path, out_path, workdir, port = sys.argv[1:6]
    port = int(port)
    rows = [json.loads(line) for line in open(raw_path)]
    with open(out_path, "w") as out:
        for net, nets in (("regtest", ("regtest", "testnet")), ("main", ("main",))):
            for service in (False, True):
                datadir = os.path.join(workdir, f"dd-{net}-{int(service)}")
                os.makedirs(datadir, mode=0o700, exist_ok=False)
                args = [dashd, f"-datadir={datadir}", *NET_FLAG[net], "-server", "-connect=0",
                        "-listen=0", "-dnsseed=0", "-fixedseeds=0", "-disablewallet",
                        "-rpcuser=fx", "-rpcpassword=fx", f"-rpcport={port}",
                        "-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1", f"-port={port + 1}"]
                if service:
                    args.append("-deprecatedrpc=service")
                proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    for _ in range(240):
                        try:
                            if rpc(port, "getblockcount").get("error") is None:
                                break
                        except OSError:
                            pass
                        time.sleep(0.5)
                    version = rpc(port, "getnetworkinfo")["result"]["subversion"]
                    if net == "regtest":
                        mined = rpc(port, "generatetodescriptor", 1, "raw(51)")
                        assert mined.get("error") is None, mined
                    for row in rows:
                        if row["net"] not in nets:
                            continue
                        reply = rpc_raw(port, "decoderawtransaction", row["hex"])
                        out.write(json.dumps({
                            "net": row["net"], "service": service, "bucket": row["bucket"],
                            "source": row["source"], "dashd": f"{version} -{net}", "hex": row["hex"],
                            "reply": reply.strip(),
                        }) + "\n")
                finally:
                    rpc(port, "stop")
                    proc.wait(timeout=120)
                print(net, service, "done", flush=True)


if __name__ == "__main__":
    main()
