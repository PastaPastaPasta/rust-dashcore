"""Regtest fixture: a small chain plus a set of pre-signed transactions.

Built once with the pinned dashd (which has a wallet) and then shared by every
implementation, so both nodes see byte-identical blocks and transactions:

    <ROOT>/data/fixtures/regtest-v1/
        chain/       dashd datadir holding the fixture chain (served by a seed)
        txs.jsonl    {"txid", "hex", "vsize"} — independent 1-in/2-out P2PKH
                     spends of confirmed outputs, valid on top of the chain tip
        meta.json    height, tip, counts, generator version

Nodes under test never open chain/ directly: they sync it from a dashd seed
over localhost (see regtest_node), which works for any implementation.
"""

import fcntl
import functools
import json
import os
import shutil
import tempfile
import time

from . import paths
from .impls import Node, NodeConfig, make_impl
from .scenarios import common

FIXTURE = "regtest-v1"
N_TXS = 30000
FANOUT = 500  # outputs per fan-out transaction
UTXO_VALUE = 1.0  # DASH per fixture input
FEE = 0.0001  # DASH per fixture transaction (~226 bytes, well above min relay fee)
# Regtest turns on consistency checks by default (fDefaultConsistencyChecks): a
# full mempool check after every accepted tx makes acceptance O(mempool size).
# Mainnet behaviour is what we benchmark, so switch them off.
REGTEST_EXTRA = ["-maxtipage=2000000000", "-persistmempool=0", "-checkmempool=0", "-checkblockindex=0"]


def fixture_dir():
    return paths.data_dir("fixtures", FIXTURE)


def load_meta():
    with open(os.path.join(fixture_dir(), "meta.json")) as f:
        return json.load(f)


def load_txs():
    with open(os.path.join(fixture_dir(), "txs.jsonl")) as f:
        return [json.loads(line) for line in f]


def ensure(ctx):
    d = fixture_dir()
    os.makedirs(os.path.dirname(d), exist_ok=True)
    with open(d + ".lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)  # concurrent first runs: one builds, the others wait
        if os.path.exists(os.path.join(d, "meta.json")):
            return load_meta()
        return build(ctx, d)


def build(ctx, d):
    ctx.log(f"building regtest fixture {d} ({N_TXS} txs) with the pinned dashd")
    tmp = tempfile.mkdtemp(prefix=os.path.basename(d) + ".", dir=os.path.dirname(d))
    cfg = NodeConfig(chain="regtest", datadir=os.path.join(tmp, "chain"), txindex=True, wallet=True,
                     dbcache_mib=1024, extra=REGTEST_EXTRA + ["-fallbackfee=0.0001"])
    gen = Node(make_impl("dashd", ctx.args), cfg, cpu_slice=ctx.aux_slice)
    with gen:
        gen.wait_ready(600)
        r = gen.rpc
        r.createwallet("gen")
        w = gen.new_rpc(wallet="gen")
        mine_to = w.getnewaddress()
        n_fanouts = -(-N_TXS // FANOUT)
        # Enough mature coinbase value for every fan-out (regtest subsidy is large).
        r.generatetoaddress(110 + n_fanouts, mine_to)
        # Fan out, locking every fixture output at once so later sendmany calls
        # cannot select (spend) it.
        utxos, inputs = [], []
        for i in range(n_fanouts):
            addrs = w.batch([("getnewaddress", [])] * FANOUT)
            txid = w.sendmany("", {a: UTXO_VALUE for a in addrs})
            outs = [{"txid": txid, "vout": v["n"]} for v in w.getrawtransaction(txid, True)["vout"]
                    if abs(v["value"] - UTXO_VALUE) < 1e-9]
            w.lockunspent(False, outs)
            utxos.append(txid)
            inputs += [(o["txid"], o["vout"]) for o in outs]
            if i % 10 == 9:
                r.generatetoaddress(1, mine_to)
        r.generatetoaddress(1, mine_to)
        inputs = inputs[:N_TXS]
        dest_a, dest_b = w.getnewaddress(), w.getnewaddress()
        change = round(UTXO_VALUE - FEE - 0.1, 8)
        txs = []
        for i in range(0, len(inputs), 500):
            chunk = inputs[i:i + 500]
            raws = w.batch([("createrawtransaction", [[{"txid": t, "vout": n}], {dest_a: 0.1, dest_b: change}])
                            for t, n in chunk])
            signed = w.batch([("signrawtransactionwithwallet", [h]) for h in raws])
            for s in signed:
                if not s["complete"]:
                    raise RuntimeError(f"fixture tx did not sign: {s}")
            decoded = w.batch([("decoderawtransaction", [s["hex"]]) for s in signed])
            txs += [{"txid": d["txid"], "hex": s["hex"], "size": d["size"]}
                    for d, s in zip(decoded, signed, strict=True)]
            ctx.log(f"  signed {len(txs)}/{len(inputs)}")
        # Sanity: the node accepts a sample (then we throw that node state away).
        test = r.testmempoolaccept([t["hex"] for t in txs[:25]])
        if not all(x["allowed"] for x in test):
            raise RuntimeError(f"fixture txs rejected: {test}")
        meta = {"fixture": FIXTURE, "height": r.getblockcount(), "tip": r.getbestblockhash(),
                "n_txs": len(txs), "fee": FEE, "generator": gen.impl.describe(),
                "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "fanout_txids": utxos}
    with open(os.path.join(tmp, "txs.jsonl"), "w") as f:
        for t in txs:
            f.write(json.dumps(t) + "\n")
    with open(os.path.join(tmp, "meta.json"), "w") as f:
        json.dump(meta, f, indent=1)
    for junk in ("debug.log", "bench-stdout.log"):
        p = os.path.join(cfg.datadir, "regtest", junk)
        if os.path.exists(p):
            os.remove(p)
    os.rename(tmp, d)
    return meta


def sync_from_seed(ctx, cfg_overrides=None, datadir=None):
    """Create a node-under-test datadir holding the fixture chain (synced over P2P).

    Returns (datadir, meta). The datadir is reused across repetitions; every
    start uses -persistmempool=0 so each repetition begins with an empty mempool.
    """
    meta = ensure(ctx)
    datadir = datadir or os.path.join(ctx.run_dir, "datadir")
    # Exclude exact names only: LevelDB keeps its write-ahead logs in *.log files.
    skip = shutil.ignore_patterns("wallets", "debug.log", "bench-stdout.log", ".lock")
    copy = functools.partial(shutil.copytree, ignore=skip)
    with common.seed(ctx, "regtest", os.path.join(fixture_dir(), "chain"), copy=copy, dbcache=512,
                     extra=REGTEST_EXTRA) as seed:
        cfg = regtest_config(datadir, connect=[f"127.0.0.1:{seed.cfg.p2pport}"], **(cfg_overrides or {}))
        with Node(ctx.impl, cfg, cpu_slice=ctx.node_slice) as node:
            node.wait_ready(600)
            deadline = time.monotonic() + 600
            while node.rpc.getblockcount() < meta["height"]:
                if time.monotonic() > deadline:
                    raise TimeoutError("node did not sync the fixture chain from the seed")
                time.sleep(0.2)
            if node.rpc.getbestblockhash() != meta["tip"]:
                raise RuntimeError("node synced a different fixture tip")
    return datadir, meta


def regtest_config(datadir, **kw):
    base = dict(chain="regtest", datadir=datadir, txindex=True, dbcache_mib=1024, extra=list(REGTEST_EXTRA))
    base.update(kw)
    return NodeConfig(**base)


def isolated_node(ctx, datadir, **kw):
    """The node under test on its synced fixture datadir, with no peers."""
    cfg = regtest_config(datadir, connect=["0"], **kw)
    return Node(ctx.impl, cfg, cpu_slice=ctx.node_slice, wrap=getattr(ctx, "wrap", ()))
