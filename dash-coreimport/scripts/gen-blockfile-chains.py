#!/usr/bin/env python3
"""Produce Dash Core blocks directories for the ST2 block-file parity test.

Every directory this script writes is produced by a real dashd; nothing is hand-written. The
parity test (`dash-coreimport/tests/blockfiles_parity.rs`) replays the same blocks through
`dashd_store::flatfile::BlockFiles` and compares the resulting blk/rev files byte for byte.

Subcommands (each writes `<datadir>/st2-manifest.json` describing what dashd did):

- `generate`: mine a regtest chain with wallet transactions on a fresh datadir (the "base" chain).
- `export`: dump the base chain's blocks in height order to a `blocks.bin` file
  (`u32 LE length ‖ block` per block).
- `replay`: start a fresh dashd with extra flags (`-fastprune`, `-tinyblk`) and feed it the
  exported blocks with `submitblock`, one at a time, optionally stopping and restarting dashd
  right before chosen blocks. That is the save-then-connect pattern of a node that receives
  blocks one by one.
- `reindex`: run `dashd -reindex` over a copy of a datadir (rev files are rewritten from scratch,
  blk files are reused in place).
- `loadblock`: import `blocks.bin`-derived blk files with `-loadblock` into a fresh datadir (all
  blocks are saved first, then connected).

All dashd processes use their own datadir and ports, and are stopped before the script exits.
"""

import argparse
import base64
import json
import os
import random
import shutil
import struct
import subprocess
import sys
import time
import urllib.request

REGTEST_MAGIC = bytes.fromhex("fcc1b7dc")
ROTATE = 2000


class Rpc:
    def __init__(self, datadir, port):
        self.url = f"http://127.0.0.1:{port}/"
        self.cookie_path = os.path.join(datadir, "regtest", ".cookie")
        self.wallet = None
        self._id = 0

    def _auth(self):
        with open(self.cookie_path) as f:
            return base64.b64encode(f.read().strip().encode()).decode()

    def batch(self, calls, wallet=None, tolerate=()):
        body = []
        for method, params in calls:
            self._id += 1
            body.append({"jsonrpc": "1.0", "id": self._id, "method": method, "params": params})
        url = self.url + (f"wallet/{wallet}" if wallet else "")
        req = urllib.request.Request(url, data=json.dumps(body).encode(),
                                     headers={"Authorization": "Basic " + self._auth(),
                                              "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=600) as resp:
            out = json.loads(resp.read())
        out.sort(key=lambda r: r["id"])
        for r in out:
            if r.get("error") and r["error"].get("code") not in tolerate:
                raise RuntimeError(f"RPC error: {r['error']}")
        return [r["result"] for r in out]

    def call(self, method, *params, wallet=None):
        return self.batch([(method, list(params))], wallet=wallet)[0]


class Dashd:
    def __init__(self, binary, datadir, port, extra):
        self.binary, self.datadir, self.port, self.extra = binary, datadir, port, extra
        self.proc = None
        self.rpc = Rpc(datadir, port + 1)

    def start(self, more=()):
        args = [self.binary, "-regtest", f"-datadir={self.datadir}", f"-port={self.port}",
                f"-rpcport={self.port + 1}", "-listen=0", "-dnsseed=0", "-fixedseeds=0",
                "-server=1", "-printtoconsole=0", "-fallbackfee=0.00001", "-debug=0",
                *self.extra, *more]
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        deadline = time.time() + 120
        while time.time() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(f"dashd exited: {self.proc.stderr.read().decode()}")
            try:
                self.rpc.call("getblockcount")
                return
            except Exception:
                time.sleep(0.2)
        raise RuntimeError("dashd did not start")

    def wait_idle(self, target_height, timeout=4 * 3600):
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                # Without peers the height reaches the target only at the end of the import.
                info = self.rpc.call("getblockchaininfo")
                if info["blocks"] >= target_height:
                    return info
            except Exception:
                pass
            # Each poll takes cs_main; do not slow the import down.
            time.sleep(10)
        raise RuntimeError("timeout waiting for height")

    def stop(self):
        if self.proc is None:
            return
        try:
            self.rpc.call("stop")
        except Exception:
            self.proc.terminate()
        self.proc.wait(timeout=300)
        if self.proc.returncode != 0:
            raise RuntimeError(f"dashd exited with {self.proc.returncode}")
        self.proc = None


def version_of(binary):
    out = subprocess.run([binary, "-version"], capture_output=True, text=True).stdout
    return out.splitlines()[0].strip()


def resumes_of(datadir):
    history = os.path.join(datadir, "st2-resumes.json")
    return json.load(open(history)) if os.path.exists(history) else []


def write_manifest(datadir, manifest):
    with open(os.path.join(datadir, "st2-manifest.json"), "w") as f:
        json.dump(manifest, f, indent=2)


def fresh(datadir):
    if os.path.exists(datadir):
        raise SystemExit(f"{datadir} exists; refusing to overwrite")
    os.makedirs(datadir)


def cmd_generate(a):
    resumed_at = None
    if a.resume and os.path.isdir(a.datadir):
        if os.path.exists(os.path.join(a.datadir, "st2-manifest.json")):
            raise SystemExit(f"{a.datadir} is complete")
    else:
        fresh(a.datadir)
    rng = random.Random(a.seed)
    # Wallet syncs are fsyncs; on a slow-fsync disk they dominate. The wallets are throwaway.
    walletdir = a.walletdir or os.path.join(a.datadir, "wallets")
    os.makedirs(walletdir, exist_ok=True)
    node = Dashd(a.dashd, a.datadir, a.port, [f"-walletdir={walletdir}"])
    node.start()
    try:
        rpc = node.rpc
        # Coinbases and most payments go to addresses of a wallet that is then unloaded, and the
        # spending wallet is replaced every `ROTATE` blocks: a legacy wallet's coin selection walks
        # its whole transaction history, which otherwise makes every send slower.
        height = rpc.call("getblockcount")
        rpc.call("loadwallet" if height else "createwallet", "sink")
        sinks = [rpc.call("getnewaddress", wallet="sink") for _ in range(20)]
        sink_pub = rpc.call("getaddressinfo", sinks[0], wallet="sink")["pubkey"]
        sinks.append(rpc.call("createmultisig", 1, [sink_pub])["address"])  # P2SH
        rpc.call("unloadwallet", "sink")
        txs = 0
        if height == 0:
            wallet = "w0"
            rpc.call("createwallet", wallet)
            rpc.call("generatetoaddress", 200, rpc.call("getnewaddress", wallet=wallet))
            height = 200
        else:
            # Resume: regtest's subsidy has halved away, so fund a new wallet from w0, which
            # kept a fifth of the first coinbases at its rotation.
            resumed_at = height
            history = os.path.join(a.datadir, "st2-resumes.json")
            resumes = json.load(open(history)) if os.path.exists(history) else []
            with open(history, "w") as f:
                json.dump(resumes + [height], f)
            rpc.call("loadwallet", "w0")
            wallet = f"r{height}"
            rpc.call("createwallet", wallet)
            fund = [rpc.call("getnewaddress", wallet=wallet) for _ in range(20)]
            each = round(float(rpc.call("getbalance", wallet="w0")) * 0.8 / 20, 8)
            rpc.call("sendmany", "", {addr: each for addr in fund}, wallet="w0")
            rpc.call("unloadwallet", "w0")
            txs += 1
            rpc.call("generatetoaddress", 1, rng.choice(sinks[:-1]))
            height += 1
        own = [rpc.call("getnewaddress", wallet=wallet) for _ in range(30)]
        while height < a.blocks:
            if height % ROTATE == 0:
                new = f"w{height // ROTATE}"
                rpc.call("createwallet", new)
                new_own = [rpc.call("getnewaddress", wallet=new) for _ in range(30)]
                # Move most of the old wallet's funds (its large change outputs) to the new one.
                each = round(float(rpc.call("getbalance", wallet=wallet)) * 0.8 / 20, 8)
                rpc.call("sendmany", "", {addr: each for addr in new_own[:20]}, wallet=wallet)
                rpc.call("unloadwallet", wallet)
                wallet, own = new, new_own
                txs += 1
                # The new wallet can spend its funds once they confirm.
                rpc.call("generatetoaddress", 1, rng.choice(sinks[:-1]))
                height += 1
            if height == a.big_block_at:
                burst = 400
            elif height % 1000 == 0:
                burst = 60
            else:
                burst = rng.choice([0, 0, 1, 1, 2, 3, 4])
            calls = []
            for _ in range(burst):
                outs = {}
                for _ in range(rng.randint(1, 3)):
                    to = rng.choice(own) if rng.random() < 0.3 else rng.choice(sinks)
                    outs[to] = round(rng.uniform(0.001, 0.2), 8)
                calls.append(("sendmany", ["", outs]))
            if calls:
                # A burst can outrun the wallet's confirmed coins (-6 insufficient funds, -6/-26
                # mempool chain limits): keep what was accepted.
                done = rpc.batch(calls, wallet=wallet, tolerate=(-6, -26))
                txs += sum(r is not None for r in done)
            rpc.call("generatetoaddress", 1, rng.choice(sinks[:-1]))
            height += 1
            if height % 2000 == 0:
                print(f"height {height}, {txs} txs", file=sys.stderr, flush=True)
        best = rpc.call("getbestblockhash")
    finally:
        node.stop()
    write_manifest(a.datadir, {
        "kind": "generate", "dashd": version_of(a.dashd), "flags": [], "height": a.blocks,
        "best": best, "seed": a.seed, "big_block_at": a.big_block_at, "wallet_txs": txs,
        "pattern": "save-then-connect per block (generatetoaddress)",
        # Every resume restarted dashd before the next block.
        "restarts_before": [h + 1 for h in resumes_of(a.datadir)],
        "resumed_at": resumed_at,
    })


def cmd_export(a):
    node = Dashd(a.dashd, a.datadir, a.port, ["-disablewallet"])
    node.start()
    try:
        rpc = node.rpc
        height = rpc.call("getblockcount")
        with open(a.out, "wb") as f:
            step = 500
            for start in range(0, height + 1, step):
                hs = range(start, min(height + 1, start + step))
                hashes = rpc.batch([("getblockhash", [h]) for h in hs])
                blocks = rpc.batch([("getblock", [h, 0]) for h in hashes])
                for hexblock in blocks:
                    raw = bytes.fromhex(hexblock)
                    f.write(struct.pack("<I", len(raw)) + raw)
    finally:
        node.stop()


def read_blocks(path):
    with open(path, "rb") as f:
        data = f.read()
    out, pos = [], 0
    while pos < len(data):
        (n,) = struct.unpack_from("<I", data, pos)
        out.append(data[pos + 4:pos + 4 + n])
        pos += 4 + n
    return out


def rollover_heights(blocks, max_size, every):
    """Heights whose block makes Core's FindBlockPos (-fastprune: 64 KiB files) leave the current
    file; returns every `every`-th one. Mirrors `nSize + nAddSize >= max_blockfile_size`."""
    out, size, n = [], 0, 0
    for h, b in enumerate(blocks):
        add = len(b) + 8
        if size + add >= max(max_size, add + 1):
            n += 1
            if h > 1 and n % every == 0:
                out.append(h)
            size = 0
        size += add
    return out


def blk_files(datadir):
    d = os.path.join(datadir, "regtest", "blocks")
    return sum(1 for n in os.listdir(d) if n.startswith("blk"))


def reorg(node, datadir, blocks, tip, depth):
    """Disconnects the last `depth` blocks (invalidateblock), mines a competing branch one block at
    a time until a new blk file appears (at most depth - 1 blocks, so the original branch keeps
    more work), then reconsiders the original branch, which becomes active again. Every block is
    still saved, then connected: fork blocks get undo on their branch; the original blocks keep
    theirs."""
    rpc = node.rpc
    base = rpc.call("getblockhash", tip - depth + 1)
    rpc.call("invalidateblock", base)
    files = blk_files(datadir)
    fork = []
    while len(fork) < depth - 1 and blk_files(datadir) == files:
        fork += rpc.call("generatetodescriptor", 1, "raw(51)")
    rolled = blk_files(datadir) > files
    if not rolled:
        # Without a rollover the case cannot tell the releases' finalize rules apart.
        raise RuntimeError(f"the fork did not roll the blk file over within {depth - 1} blocks")
    rpc.call("reconsiderblock", base)
    want = rpc.call("getblockhash", tip)
    if rpc.call("getbestblockhash") != want:
        raise RuntimeError("the original branch did not become active again")
    return {"tip": tip, "depth": depth, "fork_blocks": len(fork), "rolled_over": rolled}


def cmd_replay(a):
    fresh(a.datadir)
    blocks = read_blocks(a.blocks)
    stop = min(a.stop_height if a.stop_height is not None else len(blocks) - 1, len(blocks) - 1)
    restarts = set(a.restart_before or [])
    if a.restart_before_rollover:
        restarts |= set(h for h in rollover_heights(blocks, 0x10000, a.restart_before_rollover) if h <= stop)
    reorg_tip = None
    if a.reorg_before_rollover:
        # The last block before a -fastprune rollover: the current file is nearly full.
        rolls = [r for r in rollover_heights(blocks, 0x10000, 1) if a.reorg_depth + 1 < r <= stop]
        reorg_tip = rolls[a.reorg_before_rollover - 1] - 1
    reorg_done = None
    pruned = None
    node = Dashd(a.dashd, a.datadir, a.port, a.flag)
    node.start()
    try:
        h = 1
        while h <= stop:
            if h in restarts:
                node.stop()
                node.start()
            end = min(stop + 1, h + 200)
            nxt = [r for r in restarts if h < r < end]
            if reorg_tip is not None and h <= reorg_tip < end - 1:
                nxt.append(reorg_tip + 1)
            if nxt:
                end = min(nxt)
            res = node.rpc.batch([("submitblock", [blocks[i].hex()]) for i in range(h, end)])
            bad = [(h + i, r) for i, r in enumerate(res) if r is not None]
            if bad:
                raise RuntimeError(f"submitblock rejected: {bad[:3]}")
            h = end
            if h - 1 == reorg_tip:
                reorg_done = reorg(node, a.datadir, blocks, reorg_tip, a.reorg_depth)
        if a.prune_at is not None:
            # Core clamps to tip - MIN_BLOCKS_TO_KEEP and returns the height pruned to.
            pruned = {"requested": a.prune_at, "result": node.rpc.call("pruneblockchain", a.prune_at)}
        best = node.rpc.call("getbestblockhash")
        assert node.rpc.call("getblockcount") == stop
    finally:
        node.stop()
    write_manifest(a.datadir, {
        "kind": "replay", "dashd": version_of(a.dashd), "flags": a.flag, "height": stop,
        "best": best, "source": os.path.abspath(a.blocks),
        "pattern": "save-then-connect per block (submitblock)",
        "restarts_before": sorted(restarts), "reorg": reorg_done, "prune": pruned,
    })


def cmd_reindex(a):
    fresh(a.datadir)
    src = os.path.join(a.source, "regtest")
    dst = os.path.join(a.datadir, "regtest")
    os.makedirs(dst)
    shutil.copytree(os.path.join(src, "blocks"), os.path.join(dst, "blocks"))
    with open(os.path.join(a.source, "st2-manifest.json")) as f:
        src_manifest = json.load(f)
    node = Dashd(a.dashd, a.datadir, a.port, a.flag)
    node.start(["-reindex"])
    try:
        node.wait_idle(src_manifest["height"])
        best = node.rpc.call("getbestblockhash")
    finally:
        node.stop()
    write_manifest(a.datadir, {
        "kind": "reindex", "dashd": version_of(a.dashd), "flags": a.flag,
        "height": src_manifest["height"], "best": best, "source": os.path.abspath(a.source),
        "pattern": "known positions saved in scan order, then connect in height order",
        "restarts_before": [],
    })


def cmd_loadblock(a):
    fresh(a.datadir)
    blocks = read_blocks(a.blocks)
    stop = a.stop_height if a.stop_height is not None else len(blocks) - 1
    bootstrap = os.path.join(a.datadir, "bootstrap.dat")
    with open(bootstrap, "wb") as f:
        for b in blocks[:stop + 1]:
            f.write(REGTEST_MAGIC + struct.pack("<I", len(b)) + b)
    node = Dashd(a.dashd, a.datadir, a.port, a.flag)
    node.start([f"-loadblock={bootstrap}"])
    try:
        node.wait_idle(stop)
        best = node.rpc.call("getbestblockhash")
    finally:
        node.stop()
    os.remove(bootstrap)
    write_manifest(a.datadir, {
        "kind": "loadblock", "dashd": version_of(a.dashd), "flags": a.flag, "height": stop,
        "best": best, "source": os.path.abspath(a.blocks),
        "pattern": "all blocks saved in file order, then connected in height order",
        "restarts_before": [],
    })


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    for name in ["generate", "export", "replay", "reindex", "loadblock"]:
        s = sub.add_parser(name)
        s.add_argument("--dashd", required=True)
        s.add_argument("--datadir", required=True)
        s.add_argument("--port", type=int, required=True, help="P2P port; RPC uses port + 1")
        s.add_argument("--flag", action="append", default=[], help="extra dashd flag (repeatable)")
        if name == "generate":
            s.add_argument("--blocks", type=int, default=40000)
            s.add_argument("--seed", type=int, default=1)
            s.add_argument("--big-block-at", type=int, default=39990,
                           help="height whose block carries ~400 transactions (> 64 KiB)")
            s.add_argument("--walletdir", help="wallet directory (e.g. on /dev/shm)")
            s.add_argument("--resume", action="store_true",
                           help="continue an interrupted run in the same datadir")
        if name == "export":
            s.add_argument("--out", required=True)
        if name in ("replay", "loadblock"):
            s.add_argument("--blocks", required=True, help="blocks.bin from `export`")
            s.add_argument("--stop-height", type=int)
        if name == "replay":
            s.add_argument("--restart-before", type=int, action="append")
            s.add_argument("--restart-before-rollover", type=int, metavar="K",
                           help="restart dashd before every K-th -fastprune file rollover")
            s.add_argument("--reorg-before-rollover", type=int, metavar="K",
                           help="reorg away and back right before the K-th -fastprune rollover")
            s.add_argument("--reorg-depth", type=int, default=10)
            s.add_argument("--prune-at", type=int, metavar="HEIGHT",
                           help="pruneblockchain HEIGHT at the end (needs --flag=-prune=1)")
        if name == "reindex":
            s.add_argument("--source", required=True, help="datadir whose blocks/ is reindexed")
    a = p.parse_args()
    {"generate": cmd_generate, "export": cmd_export, "replay": cmd_replay,
     "reindex": cmd_reindex, "loadblock": cmd_loadblock}[a.cmd](a)


if __name__ == "__main__":
    main()
