#!/usr/bin/env python3
"""Extract Dash Core v24 wire vectors from regtest chains left by Core's functional tests.

For every functional-test datadir (node0), restart dashd on a copy with the chain parameters the
test ran it with, walk the active chain and keep, per distinct special-transaction shape, the first
few transactions: raw hex as stored in the block plus Core's own verbose JSON (getblock verbosity 2
entry) and what their payload signatures can be checked against. Also keep the blocks carrying
them, and masternode list diffs (P2P `mnlistdiff` from genesis, plus `protx diff`) at the heights
that cover the most kinds of address (IPv4, IPv6, Tor, I2P, domain) and version 3 masternodes.

Usage: extract.py <dashd-bin-dir> <work-dir>
Reads <work-dir>/ft (see run-functional-tests.sh), writes <work-dir>/vectors-raw.json and
<work-dir>/mnlistdiffs.json.
"""
import base64
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chainargs  # noqa: E402
import p2p_capture  # noqa: E402

PER_SHAPE = 2
PAYLOAD_KEYS = {
    1: "proRegTx", 2: "proUpServTx", 3: "proUpRegTx", 4: "proUpRevTx", 5: "cbTx", 6: "qcTx",
    7: "mnhfTx", 8: "assetLockTx", 9: "assetUnlockTx", 10: "proDisTx", 11: "proUpShareTx",
    12: "proUpSharedRegTx",
}


class Rpc:
    def __init__(self, port, cookie):
        self.url = f"http://127.0.0.1:{port}"
        self.auth = base64.b64encode(open(cookie).read().strip().encode()).decode()

    def __call__(self, method, *params):
        req = urllib.request.Request(self.url, json.dumps({"id": 0, "method": method, "params": list(params)}).encode(),
                                     {"Authorization": f"Basic {self.auth}", "Content-Type": "application/json"})
        try:
            res = json.load(urllib.request.urlopen(req, timeout=60))
        except urllib.error.HTTPError as e:  # dashd answers RPC errors with HTTP 500 and a JSON body
            res = json.load(e)
        if res.get("error"):
            raise RuntimeError(res["error"])
        return res["result"]


def shape(tx):
    """A key telling apart the wire layouts of special transactions."""
    t = tx.get("type", 0)
    if t == 0:
        return None
    p = tx.get(PAYLOAD_KEYS.get(t, ""), {})
    key = [t, p.get("version")]
    if t in (1, 2):
        key.append(p.get("type"))  # masternode type (Evo carries platform fields)
    if t in (1, 3):
        key.append("payouts" if p.get("payouts") else "legacy-payout")
    if t == 1:
        key.append(f"shares{len(p.get('shares', []))}" if p.get("shares") else "no-shares")
        key.append("external-collateral" if p.get("collateralHash", "0" * 64) != "0" * 64 else "internal")
    if t in (1, 2):
        addrs = p.get("addresses") or {}
        kinds = address_kinds([addrs])
        key.append("addresses:" + ",".join(sorted(addrs)) + "/" + ",".join(sorted(kinds)))
        key.append("multi" if any(len(v) > 1 for v in addrs.values()) else "single")
    if t == 5:
        key.append("asset-unlock-root" if int(p.get("merkleRootAssetUnlocks", "0") or "0", 16) else "")
    if t in (10, 12):
        key.append(f"sigs{p.get('sigCount')}")
    if t == 8:
        key.append("p2sh" if any(o.get("scriptPubKey", {}).get("type") == "scripthash"
                                 for o in p.get("creditOutputs", [])) else "p2pkh")
    return json.dumps(key)


def signers(rpc, tx, block_hash, scripts):
    """What the payload signature can be checked against: the collateral script of a ProRegTx
    with external collateral, the owner of a ProUpRegTx, the share owners of types 11 and 12."""
    t = tx.get("type", 0)
    p = tx.get(PAYLOAD_KEYS.get(t, ""), {})
    if t == 1 and p.get("collateralHash", "0" * 64) != "0" * 64:
        return {"collateralScript": scripts[p["collateralHash"]][p["collateralIndex"]]}
    if t in (3, 11, 12):
        state = rpc("protx", "info", p["proTxHash"], block_hash)["state"]
        if t == 3:
            return {"ownerAddress": state["ownerAddress"]}
        return {"shareOwnerAddresses": [share["ownerAddress"] for share in state["shares"]]}
    return None


def dashd_version(bindir):
    """The first line of `dashd -version`, naming the release."""
    return subprocess.run([f"{bindir}/dashd", "-version"], capture_output=True, text=True,
                          check=True).stdout.splitlines()[0]


def start_node(bindir, src, dd, port, extra_args=()):
    shutil.rmtree(dd, ignore_errors=True)
    shutil.copytree(src, dd)
    args = chainargs.last_start_args(os.path.join(dd, "regtest", "debug.log"))
    subprocess.run([f"{bindir}/dashd", f"-datadir={dd}", f"-port={port + 1}", f"-rpcport={port}", "-listen=1",
                    "-connect=0", "-checkblocks=1", "-checklevel=0", "-daemon=1", "-printtoconsole=0", *args, *extra_args],
                   check=True)
    cookie = os.path.join(dd, "regtest", ".cookie")
    for _ in range(120):
        try:
            rpc = Rpc(port, cookie)
            rpc("getblockcount")
            return rpc
        except Exception:
            time.sleep(1)
    raise RuntimeError(f"dashd in {dd} did not start")


def address_kinds(entries):
    """The kinds of address (ipv4, ipv6, tor, i2p, domain) in Core's `addresses` objects."""
    kinds = set()
    for addresses in entries:
        for rendered in (addresses or {}).values():
            for entry in rendered:
                host = entry.rsplit(":", 1)[0]
                if host.startswith("["):
                    kinds.add("ipv6")
                elif host.endswith(".onion"):
                    kinds.add("tor")
                elif host.endswith(".i2p"):
                    kinds.add("i2p")
                elif host.replace(".", "").isdigit():
                    kinds.add("ipv4")
                else:
                    kinds.add("domain")
    return kinds


def capture_mnlistdiffs(rpc, p2p_port, test, candidates):
    """Diffs from genesis: to the candidate height whose masternodes have the most kinds of
    address (then the most version 3 masternodes), and to further heights while they add kinds."""
    genesis = rpc("getblockhash", 0)
    scored = {}
    for h in sorted(set(candidates)):
        diff = rpc("protx", "diff", genesis, rpc("getblockhash", h), True)
        kinds = address_kinds(m.get("addresses") for m in diff["mnList"])
        if diff["mnList"]:
            scored[h] = ((len(kinds), sum(m["nVersion"] >= 3 for m in diff["mnList"]), len(diff["mnList"])),
                         kinds, diff)
    keep = ("nVersion", "nType", "proRegTxHash", "confirmedHash", "addresses", "pubKeyOperator", "votingAddress",
            "isValid", "platformNodeID")
    captured, covered = [], set()
    for h in sorted(scored, key=lambda h: (scored[h][0], -h), reverse=True):
        _, kinds, diff = scored[h]
        if captured and kinds <= covered:
            continue
        covered |= kinds
        mnlistdiff = p2p_capture.getmnlistd(p2p_port, bytes(32), diff["blockHash"])
        captured.append({"source": f"{test}@{h} (from genesis)", "mnlistdiff": mnlistdiff.hex(),
                         "core": {"nVersion": diff["nVersion"], "blockHash": diff["blockHash"],
                                  "merkleRootMNList": diff["merkleRootMNList"], "newQuorums": len(diff["newQuorums"]),
                                  "mnList": [{k: m[k] for k in keep if k in m} for m in diff["mnList"]]}})
    return captured


def main():
    bindir, work = sys.argv[1:3]
    ftdir = os.path.join(work, "ft")
    txs, blocks, diffs, seen = [], [], [], {}
    port = 27400
    for test in sorted(os.listdir(ftdir)):
        src = os.path.join(ftdir, test, "node0")
        if not os.path.isdir(src):
            continue
        port += 10
        rpc = start_node(bindir, src, os.path.join(work, "node", test), port)
        tip = rpc("getblockcount")
        scripts = {}  # txid -> output scripts, to find external collaterals
        provider_heights = [tip]
        print(f"{test}: tip {tip}", file=sys.stderr)
        for h in range(1, tip + 1):
            bh = rpc("getblockhash", h)
            b2 = rpc("getblock", bh, 2)
            keep_block = []
            for tx in b2["tx"]:
                scripts[tx["txid"]] = [o["scriptPubKey"]["hex"] for o in tx["vout"]]
                if tx.get("type") in (1, 2, 3, 12):
                    provider_heights.append(h)
                k = shape(tx)
                if k is None:
                    continue
                n = seen.get(k, 0)
                if n >= PER_SHAPE:
                    continue
                seen[k] = n + 1
                tx_shape = json.loads(k)
                if tx_shape[0] != 5:
                    keep_block.append(k)
                txs.append({"source": f"{test}@{h}", "shape": tx_shape, "block_hash": bh,
                            "txid": tx["txid"], "hex": tx["hex"], "core": tx, "signers": signers(rpc, tx, bh, scripts)})
            cbtx_version = b2["tx"][0].get("cbTx", {}).get("version")
            if keep_block or (cbtx_version == 4 and not any(b["cbtx_version"] == 4 for b in blocks)):
                blocks.append({"source": f"{test}@{h}", "height": h, "hash": bh, "merkleroot": b2["merkleroot"],
                               "cbtx_version": cbtx_version,
                               "txids": [t["txid"] for t in b2["tx"]], "hex": rpc("getblock", bh, 0)})
        diffs.extend(capture_mnlistdiffs(rpc, port + 1, test, provider_heights))
        rpc("stop")
        time.sleep(2)
    version = dashd_version(bindir)
    generator = f"{version} (`dashd -version`) on regtest via Core functional tests"
    json.dump({"generator": generator, "transactions": txs, "blocks": blocks},
              open(os.path.join(work, "vectors-raw.json"), "w"), indent=1)
    json.dump({"generator": generator + ": P2P getmnlistd and `protx diff`", "diffs": diffs},
              open(os.path.join(work, "mnlistdiffs.json"), "w"), indent=1)
    for k, n in sorted(seen.items()):
        print(n, k, file=sys.stderr)
    print(f"{len(txs)} txs, {len(blocks)} blocks, {len(diffs)} mnlistdiffs", file=sys.stderr)


if __name__ == "__main__":
    main()
