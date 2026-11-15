#!/usr/bin/env python3
"""CH5-1a fixture pre-proof driver for dashd v24.0.0-rc.5 (regtest, non-evo).

Subcommands:
  base                  build the base chain in fx/base and stop
  start <dir> [args..]  start dashd on <dir>
  stop <dir>
  rpc <dir> <method> [json args..]

Each datadir has its own RPC port: `start` takes the last `-rpcport=` among its args, or a free
one, and keeps it in <dir>/fx-rpcport, which `rpc` and `stop` read. So drivers on different
datadirs run side by side.
"""
import base64, json, os, socket, subprocess, sys, time, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.expanduser("~/.rust-dashcore-test/dashcore-24.0.0-rc.5/bin")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def port_file(datadir):
    return os.path.join(datadir, "fx-rpcport")


def rpc(datadir, method, *params, wallet=None, timeout=600):
    cookie = open(os.path.join(datadir, "regtest", ".cookie")).read().strip()
    port = int(open(port_file(datadir)).read())
    url = f"http://127.0.0.1:{port}/" + (f"wallet/{wallet}" if wallet else "")
    req = urllib.request.Request(url, data=json.dumps({"id": 1, "method": method, "params": list(params)}).encode())
    req.add_header("Authorization", "Basic " + base64.b64encode(cookie.encode()).decode())
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            body = json.load(r)
    except urllib.error.HTTPError as e:
        body = json.load(e)
    if body.get("error"):
        raise RuntimeError(f"{method}: {body['error']}")
    return body["result"]


def start(datadir, *extra):
    os.makedirs(datadir, exist_ok=True)
    given = [a.split("=", 1)[1] for a in extra if a.startswith("-rpcport=")]
    rpcport = int(given[-1]) if given else free_port()
    with open(port_file(datadir), "w") as f:
        f.write(str(rpcport))
    # `extra` comes last, so its `-rpcport` or `-port` is the one dashd takes.
    args = [f"{BIN}/dashd", "-regtest", f"-datadir={datadir}", f"-rpcport={rpcport}",
            f"-port={free_port()}", "-listen=0", "-server=1", "-daemon=0", "-printtoconsole=0",
            "-fallbackfee=0.0001", *extra]
    log = open(os.path.join(datadir, "stdout.log"), "ab")
    p = subprocess.Popen(args, stdout=log, stderr=subprocess.STDOUT)
    for _ in range(600):
        if p.poll() is not None:
            return p.returncode
        try:
            rpc(datadir, "getblockcount", timeout=5)
            return None
        except Exception:
            time.sleep(0.2)
    raise RuntimeError("dashd did not come up")


def stop(datadir):
    rpc(datadir, "stop")
    pidf = os.path.join(datadir, "regtest", "dashd.pid")
    for _ in range(600):
        if not os.path.exists(pidf):
            return
        time.sleep(0.2)
    raise RuntimeError("dashd did not stop")


def build_base(datadir):
    assert start(datadir) is None
    rpc(datadir, "createwallet", "w")
    addr = rpc(datadir, "getnewaddress", wallet="w")
    rpc(datadir, "generatetoaddress", 110, addr)
    plan = {}
    # The L4-only fixture's input: block 5's coinbase, kept out of coin selection until block 118
    # spends it, so its creating block lies below every depth short of the whole chain.
    cb5 = rpc(datadir, "getblock", rpc(datadir, "getblockhash", 5))["tx"][0]
    rpc(datadir, "lockunspent", False, [{"txid": cb5, "vout": 0}], wallet="w")
    # Blocks 111..120 each carry wallet transactions; later ones spend earlier in-range outputs.
    for h in range(111, 121):
        txids = []
        if h == 118:
            rpc(datadir, "lockunspent", True, [{"txid": cb5, "vout": 0}], wallet="w")
            to = rpc(datadir, "getnewaddress", wallet="w")
            raw = rpc(datadir, "createrawtransaction", [{"txid": cb5, "vout": 0}], {to: 499.99})
            signed = rpc(datadir, "signrawtransactionwithwallet", raw, wallet="w")
            txids.append(rpc(datadir, "sendrawtransaction", signed["hex"]))
            plan["cb5"] = cb5
        for _ in range(3):
            to = rpc(datadir, "getnewaddress", wallet="w")
            txids.append(rpc(datadir, "sendtoaddress", to, 1.5, wallet="w"))
        blk = rpc(datadir, "generateblock", addr, txids)
        plan[h] = {"hash": blk["hash"], "txids": txids}
    tip = rpc(datadir, "getblockcount")
    hashes = [rpc(datadir, "getblockhash", h) for h in range(tip + 1)]
    with open(os.path.join(HERE, "base-chain.json"), "w") as f:
        json.dump({"tip": tip, "hashes": hashes, "plan": plan}, f, indent=1)
    stop(datadir)


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "base":
        build_base(os.path.join(HERE, "base"))
    elif cmd == "start":
        print(start(sys.argv[2], *sys.argv[3:]))
    elif cmd == "stop":
        stop(sys.argv[2])
    elif cmd == "rpc":
        print(json.dumps(rpc(sys.argv[2], sys.argv[3], *[json.loads(a) for a in sys.argv[4:]]), indent=1))
