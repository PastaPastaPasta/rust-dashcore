#!/usr/bin/env python3
"""ST3b fixtures: a Core-produced regtest chain with tx churn, its per-block oracle rows and the
delivery schedule the power-loss runs replay (PLAN-ST3b §5).

A pinned dashd (v24 rc.4) mines the chain with DIP0003, V20 and MN_RR moved out of reach (CH3b
D210's arguments, so no block needs a CbTx), the block times driven by mocktime from a fixed
start. Each block carries 1-3 transactions spending confirmed coins into 2-20 outputs (fewer when
the coin pool is large, so the UTXO set stays a few thousand coins); the coins are P2SH(OP_TRUE),
so no wallet signs them. While each block is dashd's tip, the script records its row: hash,
height, prev, time, bits, then `gettxoutsetinfo muhash` (muhash, txouts, total_amount, bogosize)
and `hash_serialized_2`.

    mkfixture.py <out dir> --blocks N [--seed S] [--reorgs H:D,...] [--serve H] [--f01 P]
                 [--dashd PATH]

  --reorgs 1100:3,1350:8  at tip height H, invalidate the top D blocks and mine D+1 on the fork
                          (then reconsider, so the old branch is a valid, lighter side branch)
  --serve 1000            also keep a copy of the datadir with its tip at height H (`serve-H/`),
                          for a peer that serves the chain up to there over P2P; the final
                          datadir is always kept (`serve-final/`)
  --f01 1210              F01 (Q12): a sibling of block P+1 with a script-invalid spend, and its
                          child, PoW ground on a scratch copy of the datadir at P; dashd's own
                          verdict on them is recorded

Out: rows.jsonl, blocks.bin (each block in mining order: u32 LE length, raw bytes), events.jsonl
(mined/invalidated/reconsidered in order), schedule.json (the P2P part and the submitblock
order), trajectory.json (every active tip a node passes through on that schedule, disconnect
and connect paths included), f01.json, rpc.log, provenance.json (dashd version and sha256, args,
seed, sha256 of every file). Deterministic in the seed.
"""
import argparse
import base64
import hashlib
import http.client
import json
import os
import random
import socket
import struct
import subprocess
import sys
import time

PIN = "v24.0.0-rc.4"
CHAIN_ARGS = [
    "-dip3params=1000000:1000000",
    "-testactivationheight=v20@1000000",
    "-testactivationheight=mn_rr@1000000",
]
SPACING = 30
COINBASE_MATURITY = 100
POOL_TARGET = 2000
DUFF = 100_000_000


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def sha256d(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


class RpcError(Exception):
    def __init__(self, method, error):
        super().__init__(f"{method}: {error}")
        self.error = error


class Dashd:
    """One dashd on its own datadir and ports; every call goes to the RPC log."""

    def __init__(self, binary, datadir, log, extra=()):
        self.binary, self.datadir, self.log, self.extra = binary, datadir, log, list(extra)
        self.proc = self.conn = None

    def start(self, mocktime):
        self.rpcport, self.port = free_port(), free_port()
        args = [
            self.binary, "-regtest", f"-datadir={self.datadir}", f"-rpcport={self.rpcport}",
            f"-port={self.port}", "-server=1", "-listen=0", "-connect=0", "-dnsseed=0",
            "-printtoconsole=0", "-fallbackfee=0.00001", f"-mocktime={mocktime}", *CHAIN_ARGS,
            *self.extra,
        ]
        self.log.write(json.dumps({"start": args[1:]}) + "\n")
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        cookie = os.path.join(self.datadir, "regtest", ".cookie")
        for _ in range(600):
            if self.proc.poll() is not None:
                sys.exit(f"dashd exited {self.proc.returncode} on start; see {self.datadir}")
            try:
                self.auth = "Basic " + base64.b64encode(open(cookie, "rb").read().strip()).decode()
                self.call("getblockcount", log=False)
                return
            except (OSError, RpcError):
                time.sleep(0.2)
        sys.exit("dashd did not come up")

    def call(self, method, *params, log=True):
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)})
        for attempt in (0, 1):
            if self.conn is None:
                self.conn = http.client.HTTPConnection("127.0.0.1", self.rpcport, timeout=600)
            try:
                self.conn.request("POST", "/", body, {"Authorization": self.auth})
                reply = json.loads(self.conn.getresponse().read())
                break
            except (http.client.HTTPException, ConnectionError):
                # A kept-alive connection the server closed: once more on a fresh one.
                self.conn.close()
                self.conn = None
                if attempt:
                    raise
        if log:
            self.log.write(json.dumps({"rpc": method, "params": list(params)}) + "\n")
        if reply.get("error"):
            raise RpcError(method, reply["error"])
        return reply["result"]

    def stop(self):
        self.call("stop")
        self.proc.wait(timeout=600)
        self.conn.close()
        self.proc = self.conn = None


def le_num(n):
    """A CScriptNum push of n (BIP34's height)."""
    if n == 0:
        return b"\x00"
    out = bytearray()
    while n:
        out.append(n & 0xFF)
        n >>= 8
    if out[-1] & 0x80:
        out.append(0)
    return bytes([len(out)]) + bytes(out)


def varint(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    return b"\xfe" + struct.pack("<I", n)


def coinbase(height, outputs, tag):
    """A pre-DIP3 coinbase: version 1, BIP34 height, `outputs` [(duffs, script bytes)]."""
    script = le_num(height) + bytes([len(tag)]) + tag
    tx = struct.pack("<I", 1) + b"\x01" + b"\x00" * 32 + b"\xff" * 4
    tx += varint(len(script)) + script + b"\xff" * 4
    tx += varint(len(outputs))
    for value, spk in outputs:
        tx += struct.pack("<q", value) + varint(len(spk)) + spk
    return tx + b"\x00" * 4


def merkle_root(txs):
    layer = [sha256d(t) for t in txs]
    while len(layer) > 1:
        if len(layer) % 2:
            layer.append(layer[-1])
        layer = [sha256d(layer[i] + layer[i + 1]) for i in range(0, len(layer), 2)]
    return layer[0]


def header(version, prev_hex, root, ntime, bits_hex, nonce):
    return (
        struct.pack("<i", version) + bytes.fromhex(prev_hex)[::-1] + root
        + struct.pack("<II", ntime, int(bits_hex, 16)) + struct.pack("<I", nonce)
    )


# P2SH(OP_TRUE): standard to spend and to relay, and spent with a fixed scriptSig, so the churn
# needs no wallet and no signatures.
REDEEM = b"\x51"
SPEND_SIG = bytes([len(REDEEM)]) + REDEEM
# F01's spend: a push of another redeem script, whose hash is not the coin's (the scriptPubKey
# evaluates false under any flags, a mandatory script failure).
BAD_SIG = b"\x01\x00"


def hash160(b):
    return hashlib.new("ripemd160", hashlib.sha256(b).digest()).digest()


P2SH_TRUE = b"\xa9\x14" + hash160(REDEEM) + b"\x87"


def tx_bytes(ins, outs, script_sig=SPEND_SIG):
    """A version-2 normal transaction spending `ins` [(txid hex, vout)] into `outs` [(duffs,
    script)]."""
    tx = struct.pack("<I", 2) + varint(len(ins))
    for txid, vout in ins:
        tx += bytes.fromhex(txid)[::-1] + struct.pack("<I", vout)
        tx += varint(len(script_sig)) + script_sig + b"\xff" * 4
    tx += varint(len(outs))
    for value, spk in outs:
        tx += struct.pack("<q", value) + varint(len(spk)) + spk
    return tx + b"\x00" * 4


class Generator:
    def __init__(self, a):
        self.a, self.out = a, a.out
        self.rng = random.Random(a.seed)
        self.rpclog = open(os.path.join(self.out, "rpc.log"), "w")
        self.rows = open(os.path.join(self.out, "rows.jsonl"), "w")
        self.blocks = open(os.path.join(self.out, "blocks.bin"), "wb")
        self.events = open(os.path.join(self.out, "events.jsonl"), "w")
        self.datadir = os.path.join(self.out, "gen")
        os.makedirs(self.datadir, mode=0o700)
        self.node = Dashd(a.dashd, self.datadir, self.rpclog, ["-disablewallet"])
        # Fixed, so the fixture does not depend on the day it is made; the harness gives rdashd
        # -mocktime from the schedule.
        self.t0 = 1_700_000_000
        self.height = 0
        self.mined = []  # hashes in mining order
        self.pool = {}  # (txid, vout) -> duffs: spendable, confirmed and mature
        self.pending_cb = {}  # height -> coinbase txid on the active chain, not yet mature
        self.sent = {}  # txid -> [(vout, duffs)] in the mempool

    def mocktime(self, height):
        return self.t0 + height * SPACING

    def snapshot(self, name):
        self.node.stop()
        dst = os.path.join(self.out, name)
        subprocess.run(["cp", "-a", "--reflink=always", self.datadir, dst], check=True)
        self.node.start(self.mocktime(self.height))

    def mine_one(self, churn=True, nudge=0):
        n = self.node
        n.call("setmocktime", self.mocktime(self.height + 1) + nudge)
        if churn and self.height > COINBASE_MATURITY:
            self.churn()
        [bh] = n.call("generatetoaddress", 1, self.addr)
        self.record(bh)
        return bh

    def record(self, bh):
        n = self.node
        block = n.call("getblock", bh, 1)
        mu = n.call("gettxoutsetinfo", "muhash")
        hs = n.call("gettxoutsetinfo", "hash_serialized_2")
        assert mu["bestblock"] == bh and hs["bestblock"] == bh, "recorded while it is the tip"
        row = {
            "hash": bh, "height": block["height"], "prev": block.get("previousblockhash"),
            "time": block["time"], "bits": block["bits"], "muhash": mu["muhash"],
            "txouts": mu["txouts"], "total_amount": mu["total_amount"],
            "bogosize": mu["bogosize"], "hash_serialized_2": hs["hash_serialized_2"],
        }
        self.height = row["height"]
        self.rows.write(json.dumps(row) + "\n")
        raw = bytes.fromhex(n.call("getblock", bh, 0))
        self.blocks.write(struct.pack("<I", len(raw)) + raw)
        self.events.write(json.dumps({"mined": bh, "height": row["height"]}) + "\n")
        self.mined.append(bh)
        self.pending_cb[row["height"]] = block["tx"][0]
        # A coinbase is spendable once 100 blocks are on top of it.
        txid = self.pending_cb.pop(row["height"] - COINBASE_MATURITY, None)
        if txid is not None:
            out = n.call("gettxout", txid, 0)
            if out is not None and out["scriptPubKey"]["hex"] == P2SH_TRUE.hex():
                self.pool[(txid, 0)] = round(out["value"] * DUFF)
        for txid in list(self.sent):
            if txid in block["tx"]:
                for vout, duffs in self.sent.pop(txid):
                    self.pool[(txid, vout)] = duffs

    def churn(self):
        n = self.node
        for _ in range(self.rng.randint(1, 3)):
            coins = sorted(self.pool)
            if not coins:
                return
            big = len(coins) > POOL_TARGET
            n_in = min(len(coins), self.rng.randint(4, 12) if big else self.rng.randint(1, 4))
            n_out = self.rng.randint(1, 2) if big else self.rng.randint(2, 20)
            ins = self.rng.sample(coins, n_in)
            total = sum(self.pool[c] for c in ins)
            fee = 1000 + 100 * (n_in + n_out)
            if total - fee < n_out * 10_000:
                continue
            cuts = sorted(self.rng.randint(1, total - fee - 1) for _ in range(n_out - 1))
            parts = [b - a for a, b in zip([0, *cuts], [*cuts, total - fee])]
            if min(parts) < 10_000:
                continue
            tx = tx_bytes(ins, [(v, P2SH_TRUE) for v in parts])
            txid = n.call("sendrawtransaction", tx.hex())
            assert txid == sha256d(tx)[::-1].hex()
            for c in ins:
                del self.pool[c]
            self.sent[txid] = list(enumerate(parts))

    def reorg(self, depth):
        """Invalidates the top `depth` blocks, mines `depth + 1` on the fork, then reconsiders
        the old branch, which stays a valid side branch with less work."""
        n = self.node
        tip = self.height
        first = n.call("getblockhash", tip - depth + 1)
        n.call("invalidateblock", first)
        self.events.write(json.dumps({"invalidated": first, "height": tip - depth + 1}) + "\n")
        self.height = tip - depth
        # The disconnected transactions go back to the mempool, and the fork mines them.
        for _ in range(depth + 1):
            self.mine_one(churn=False, nudge=1)
        n.call("reconsiderblock", first)
        self.events.write(json.dumps({"reconsidered": first}) + "\n")
        assert n.call("getblockcount") == tip + 1
        # Drop what the reorg changed: a coin is ours only while it is unspent, mempool included.
        self.pool = {
            c: v for c, v in self.pool.items() if n.call("gettxout", c[0], c[1], True) is not None
        }
        self.sent = {}

    def run(self):
        a, n = self.a, self.node
        n.start(self.mocktime(0))
        self.addr = n.call("decodescript", REDEEM.hex())["p2sh"]
        # Genesis's row too: every path the trajectory walks ends there.
        self.record(n.call("getbestblockhash"))
        reorgs = dict(a.reorgs)
        f01 = None
        while self.height < a.blocks:
            h = self.height
            if h in reorgs:
                self.reorg(reorgs.pop(h))
                continue
            if h == a.serve:
                self.snapshot(f"serve-{h}")
            if h == a.f01:
                self.snapshot(f"f01-scratch-{h}")
                f01 = self.f01_template()
            bh = self.mine_one()
            if f01 is not None and "next" not in f01:
                f01["next"], f01["next_hash"] = n.call("getblock", bh, 0), bh
            if self.height % 500 == 0:
                print(f"height {self.height}", flush=True)
        assert not reorgs, f"reorg heights never reached: {reorgs}"
        self.final = n.call("getbestblockhash")
        self.final_time = n.call("getblockheader", self.final)["time"]
        for f in (self.rows, self.blocks, self.events):
            f.close()
        n.stop()
        subprocess.run(
            ["cp", "-a", "--reflink=always", self.datadir, os.path.join(self.out, "serve-final")],
            check=True,
        )
        if f01:
            self.make_f01(f01)
        self.write_schedule()
        self.rpclog.close()
        self.write_provenance()

    def f01_template(self):
        """At tip P: the template for P+1 and a spend of a mature coin with the wrong redeem
        script."""
        n = self.node
        coin = min(self.pool)
        bad = tx_bytes([coin], [(self.pool[coin] - 2000, P2SH_TRUE)], BAD_SIG)
        return {
            "template": n.call("getblocktemplate", {"rules": []}), "bad_tx": bad.hex(),
            "coin": list(coin), "parent": n.call("getbestblockhash"), "height": self.height,
        }

    def make_f01(self, f):
        """On the scratch copy at P: A's block P+1, then F (its equal-work sibling, spending a
        coin with a bad signature) and F's child C, PoW ground by `submitheader` (a header with
        bad PoW is refused as high-hash before anything else is checked). dashd's answers and
        log lines on submitting F, then C, are the reference verdict."""
        t = f["template"]
        subsidy = t["coinbasevalue"] - sum(x["fee"] for x in t["transactions"])
        payees = [(p["amount"], bytes.fromhex(p["script"]))
                  for key in ("masternode", "superblock") for p in t.get(key, [])]
        ours = subsidy - sum(v for v, _ in payees)
        scratch = Dashd(self.a.dashd, os.path.join(self.out, f"f01-scratch-{f['height']}"),
                        self.rpclog, ["-disablewallet"])
        debug_log = os.path.join(scratch.datadir, "regtest", "debug.log")
        log_from = os.path.getsize(debug_log)  # the copy holds the generator's log before it
        scratch.start(t["curtime"] + 10)
        assert scratch.call("getbestblockhash") == f["parent"]
        assert scratch.call("submitblock", f["next"]) is None
        bad = bytes.fromhex(f["bad_tx"])

        def grind(prev, txs, ntime):
            """The block, and its id as dashd reports it (X11, not computed here)."""
            root = merkle_root(txs)
            for nonce in range(1 << 32):
                hdr = header(t["version"], prev, root, ntime, t["bits"], nonce)
                try:
                    scratch.call("submitheader", hdr.hex())
                except RpcError as e:
                    if "high-hash" in str(e.error):
                        continue
                    raise
                tips = scratch.call("getchaintips")
                [mine] = [x["hash"] for x in tips if x["status"] == "headers-only"]
                return mine, hdr + varint(len(txs)) + b"".join(txs)
            raise SystemExit("no nonce")

        cb_f = coinbase(f["height"] + 1, [(ours, b"\x51"), *payees], b"st3b-f01")
        f_hash, f_block = grind(f["parent"], [cb_f, bad], t["curtime"])
        cb_c = coinbase(f["height"] + 2, [(ours, b"\x51"), *payees], b"st3b-f01c")
        c_hash, c_block = grind(f_hash, [cb_c], t["curtime"] + 1)
        verdict = {"submit_f": scratch.call("submitblock", f_block.hex())}
        verdict["tips_after_f"] = scratch.call("getchaintips")
        verdict["submit_c"] = scratch.call("submitblock", c_block.hex())
        verdict["tips_after_c"] = scratch.call("getchaintips")
        verdict["best"] = scratch.call("getbestblockhash")
        scratch.stop()
        with open(debug_log, errors="replace") as log:
            log.seek(log_from)
            verdict["log"] = [
                line.rstrip("\n") for line in log
                if "script-verify" in line or "InvalidChainFound" in line or "ConnectBlock" in line
                or "ConnectTip" in line
            ]
        json.dump(
            {"parent": f["parent"], "height": f["height"], "next": f["next_hash"],
             "f": {"hash": f_hash, "hex": f_block.hex()},
             "c": {"hash": c_hash, "hex": c_block.hex()}, "coin": f["coin"],
             "verdict": verdict},
            open(os.path.join(self.out, "f01.json"), "w"), indent=1,
        )

    def write_schedule(self):
        """P2P up to the serve height, then every later block by submitblock in mining order;
        the trajectory is the active tip after each block, with the reorg paths."""
        rows = {}
        for line in open(os.path.join(self.out, "rows.jsonl")):
            r = json.loads(line)
            rows[r["hash"]] = r
        bits = {r["bits"] for r in rows.values()}
        assert len(bits) == 1, f"equal work per block assumed, bits {bits}"
        if self.a.serve is not None:
            p2p_tip = next(h for h in self.mined if rows[h]["height"] == self.a.serve)
            served = set()
            x = p2p_tip
            while x:
                served.add(x)
                x = rows[x]["prev"]
            submit = [h for h in self.mined if h not in served]
            p2p = {"datadir": f"serve-{self.a.serve}", "tip": p2p_tip}
        else:
            p2p_tip, submit = self.final, []
            p2p = {"datadir": "serve-final", "tip": self.final}

        def path(x):
            out = []
            while x:
                out.append(x)
                x = rows[x]["prev"]
            return out[::-1]

        traj = path(p2p_tip)
        tip = p2p_tip
        for h in submit:
            # Equal work per block: a block becomes the tip only with a greater height (first
            # seen wins a tie).
            if rows[h]["height"] <= rows[tip]["height"]:
                continue
            old, new = path(tip), path(h)
            fork = 0
            while fork < min(len(old), len(new)) and old[fork] == new[fork]:
                fork += 1
            traj += old[fork - 1:-1][::-1] + new[fork:]
            tip = h
        assert tip == self.final, "the schedule ends at the generator's tip"
        json.dump({"p2p": p2p, "submit": submit, "final": self.final,
                   "mocktime": self.final_time + 3600},
                  open(os.path.join(self.out, "schedule.json"), "w"), indent=1)
        json.dump(traj, open(os.path.join(self.out, "trajectory.json"), "w"))

    def write_provenance(self):
        a = self.a
        version = subprocess.run([a.dashd, "-version"], capture_output=True, text=True).stdout
        files = {}
        for name in sorted(os.listdir(self.out)):
            p = os.path.join(self.out, name)
            if os.path.isfile(p):
                files[name] = sha256_file(p)
        json.dump(
            {"dashd": {"path": a.dashd, "version": version.splitlines()[0],
                       "sha256": sha256_file(a.dashd)},
             "args": sys.argv[1:], "chain_args": CHAIN_ARGS, "seed": a.seed, "t0": self.t0,
             "spacing": SPACING, "blocks": a.blocks, "final": self.final, "files": files,
             "generator_sha256": sha256_file(__file__)},
            open(os.path.join(self.out, "provenance.json"), "w"), indent=1,
        )


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("out")
    p.add_argument("--blocks", type=int, required=True)
    p.add_argument("--seed", type=int, default=1)
    p.add_argument("--reorgs", default="",
                   type=lambda s: [tuple(map(int, x.split(":"))) for x in s.split(",") if x])
    p.add_argument("--serve", type=int)
    p.add_argument("--f01", type=int)
    p.add_argument("--dashd", default=os.path.expanduser(
        "~/.rust-dashcore-test/dashcore-24.0.0-rc.4/bin/dashd"))
    a = p.parse_args()
    version = subprocess.run([a.dashd, "-version"], capture_output=True, text=True).stdout
    if f"version {PIN}" not in version.splitlines()[0]:
        sys.exit(f"{a.dashd} is not dashd {PIN}: {version.splitlines()[:1]}")
    os.umask(0o077)
    os.makedirs(a.out, mode=0o700)
    Generator(a).run()


if __name__ == "__main__":
    main()
