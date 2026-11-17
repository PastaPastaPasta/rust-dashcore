#!/usr/bin/env python3
"""Collect Dash Core-signed special transactions plus the keys Core verified them against.

Provenance
----------
Every byte and every key in the output comes from Dash Core (dashd v24.0.0-rc.3 release
binary) over JSON-RPC; nothing is computed or hand-written here. No sighash is computed:
the fixtures only pair Core-accepted raw transactions with the signer material that was
chain state when Core accepted them, so a re-implementation of each payload's signed
message can be checked against Core's own signatures.

Sources:

* ``regtest``: chains built by Dash Core's functional tests (feature_asset_locks,
  feature_masternode_payout_shares, feature_masternode_shares, feature_protx_version,
  rpc_netinfo; node0 datadirs under /work/conformance/regtest-v24). Each datadir is copied
  (``cp -a``) to a replica directory and dashd runs on the copy with the chain-parameter
  args the test passed on its command line (recovered from the last start's
  "Command-line arg:" lines in debug.log; the logged -mocktime is left out). The original
  datadir is never opened by dashd.
* ``mainnet``: a syncing mainnet reference node, read with a restricted RPC set only
  (getblockhash, getblockheader, getblock <= 3, getbestblockhash, getblockchaininfo,
  ``protx listdiff``). An external collateral's script would need getrawtransaction, so
  mainnet samples no ProRegTx with external collateral.

For every special transaction (version 3, type 1..12) of the active chain the entry holds
the raw tx hex, Core's JSON for the payload, and type-dependent signer info looked up as
of the parent block (h-1):

* 1 ProRegTx: the external collateral output's script and address (internal collateral
  carries no payload signature; shares payloads carry their own keys);
* 2 ProUpServTx / 4 ProUpRevTx: pubKeyOperator and the full MN state (``protx info
  <proTxHash> <hash(h-1)>``);
* 3 ProUpRegTx: ownerAddress / owner key id from the MN state;
* 5 CbTx, 8 AssetLockTx: no signer (a few per chain are recorded);
* 6 QcTx: none (the quorum key is in the payload); all non-null and <= 5 null ones;
* 7 MnHfTx, 9 AssetUnlockTx: the quorum (``quorum info <type> <quorumHash>``);
* 10..12: the MN state (shares, operator key) at h-1.

Usage
-----
    python3 -I gen_sigvec.py regtest [--chains a,b] [--out DIR]
    python3 -I gen_sigvec.py mainnet [--per-kind 25] [--max-blocks 4000] [--out DIR]
    python3 -I gen_sigvec.py summary [--out DIR]

``regtest`` copies each datadir to $SIGVEC_REPLICAS/<chain> (default /dev/shm/sigvec/replicas;
dashd unpacked from the tarball under $SIGVEC_SCRATCH), starts dashd
there (own ports on 127.0.0.1, -connect=0, -rpcdoccheck=0; the copied dash.conf has bind=127.0.0.1, which rules out -listen=0), walks the chain, writes
``regtest-<chain>.json``, stops dashd via RPC ``stop`` (falling back to SIGTERM of the
exact PID) and deletes the replica. ``mainnet`` writes ``mainnet.json``.
"""
import argparse
import base64
import datetime
import hashlib
import http.client
import json
import os
import shutil
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SCRATCH = os.environ.get("SIGVEC_SCRATCH", "/work/scratch/sigvec")
DASHD_DIR = f"{SCRATCH}/dashd/dashcore-24.0.0-rc.3/bin"
TARBALL = "/work/chains/bin/dl/dashcore-24.0.0-rc.3-x86_64-linux-gnu.tar.gz"
SOURCE_ROOT = "/work/conformance/regtest-v24"
REPLICA_ROOT = os.environ.get("SIGVEC_REPLICAS", "/dev/shm/sigvec/replicas")
CHAINS = ("feature_asset_locks", "feature_masternode_payout_shares", "feature_masternode_shares",
          "feature_protx_version", "rpc_netinfo")
CHAIN_ARG_PREFIXES = ("dip3params", "vbparams", "llmqtest", "testactivationheight", "budgetparams",
                      "highsubsidy")
BASE_PORT = 41630

MAINNET = {"host": "127.0.0.1", "port": 27998, "cookie": "/work/chains/mainnet/rpc.cookie"}
# The pure reads allowed on the reference nodes (ORACLE.md); `protx` only as `listdiff`.
MAINNET_ALLOWED = {"getblockhash", "getblockheader", "getblock", "getbestblockhash", "getblockchaininfo",
                   "protx"}

TYPE_NAMES = {1: "ProRegTx", 2: "ProUpServTx", 3: "ProUpRegTx", 4: "ProUpRevTx", 5: "CbTx", 6: "QcTx",
              7: "MnHfTx", 8: "AssetLockTx", 9: "AssetUnlockTx", 10: "ProDisTx", 11: "ProUpShareTx",
              12: "ProUpSharedRegTx"}
# Keys of a getblock-2 transaction object that are not the special payload.
TX_KEYS = {"txid", "hash", "version", "type", "size", "vsize", "weight", "locktime", "vin", "vout",
           "extraPayloadSize", "extraPayload", "hex", "fee", "instantlock", "instantlock_internal",
           "chainlock", "height", "confirmations", "time", "blocktime", "blockhash"}
# Every LLMQ type in Dash Core v24 (src/llmq/params.h); `quorum info` rejects types not
# active on the chain.
LLMQ_TYPES = {1: "llmq_50_60", 2: "llmq_400_60", 3: "llmq_400_85", 4: "llmq_100_67", 5: "llmq_60_75",
              6: "llmq_25_67", 100: "llmq_test", 101: "llmq_devnet", 102: "llmq_test_v17",
              103: "llmq_test_dip0024", 104: "llmq_test_instantsend", 105: "llmq_devnet_dip0024",
              106: "llmq_test_platform", 107: "llmq_devnet_platform", 108: "llmq_single_node"}
# Regtest consensus.llmqTypeMnhf / llmqTypePlatform (no RPC prints them, and the
# functional tests used here pass no override): MnHfTx is verified by mnhftx.cpp against
# llmqTypeMnhf, AssetUnlockTx by assetlocktx.cpp against llmqTypePlatform.
REGTEST_LLMQ_MNHF = 100      # LLMQ_TEST
REGTEST_LLMQ_PLATFORM = 106  # LLMQ_TEST_PLATFORM
REGTEST_LLMQ_SOURCE = ("dash v24.0.0-rc.3 src/chainparams.cpp CRegTestParams: llmqTypeMnhf = LLMQ_TEST, "
                       "llmqTypePlatform = LLMQ_TEST_PLATFORM; used by src/evo/mnhftx.cpp:145 and "
                       "src/evo/assetlocktx.cpp:116")
NO_SIGNER_CAP = 5  # CbTx / AssetLock / null QcTx entries kept per chain
NULL_HASH = "0" * 64


class RpcError(Exception):
    pass


class Rpc:
    def __init__(self, host, port, cookie, timeout=600, allowed=None):
        self.host, self.port, self.cookie, self.timeout, self.allowed = host, port, cookie, timeout, allowed

    def __call__(self, method, *params, timeout=None):
        if self.allowed is not None and (method not in self.allowed
                                         or method == "protx" and params[:1] != ("listdiff",)):
            raise RuntimeError(f"RPC {method} not allowed on this node")
        auth = base64.b64encode(open(self.cookie, "rb").read().strip()).decode()
        conn = http.client.HTTPConnection(self.host, self.port, timeout=timeout or self.timeout)
        try:
            body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)})
            conn.request("POST", "/", body, {"Authorization": "Basic " + auth,
                                              "Content-Type": "application/json"})
            reply = json.loads(conn.getresponse().read())
        finally:
            conn.close()
        if reply.get("error"):
            raise RpcError(f"{method}{list(params)}: {reply['error']}")
        return reply["result"]


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")


def payload_of(tx):
    """(key, object) of Core's JSON for the special payload of a getblock-2 transaction."""
    extra = [k for k in tx if k not in TX_KEYS and isinstance(tx[k], dict)]
    if len(extra) != 1:
        return None, None
    return extra[0], tx[extra[0]]


def chain_args(debug_log):
    """Chain-parameter args of the last dashd start logged in debug_log (no -mocktime)."""
    starts, current = [], []
    for line in open(debug_log, errors="replace"):
        if "Dash Core version" in line and current:
            starts.append(current)
            current = []
        if "Command-line arg: " in line:
            name, _, value = line.split("Command-line arg: ", 1)[1].strip().partition("=")
            if name.startswith(CHAIN_ARG_PREFIXES):
                current.append(f"-{name}={value.strip(chr(34))}")
    if current:
        starts.append(current)
    return starts[-1] if starts else []


# --------------------------------------------------------------------------------------- regtest

def free_port_pair(start):
    """First port p >= start such that p and p+1 can be bound on 127.0.0.1 and ::1."""
    import socket
    for p in range(start, 41999):
        try:
            for port in (p, p + 1):
                for family, host in ((socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")):
                    with socket.socket(family, socket.SOCK_STREAM) as sock:
                        sock.bind((host, port))
            return p
        except OSError:
            continue
    raise RuntimeError("no free port pair in 41000-41999")


class Node:
    """dashd running on a replica of a functional-test datadir."""

    def __init__(self, chain, index):
        self.chain = chain
        self.src = os.path.join(SOURCE_ROOT, f"{chain}-node0")
        self.dd = os.path.join(REPLICA_ROOT, chain)
        self.rpcport = free_port_pair(BASE_PORT + 10 * index)
        self.port = self.rpcport + 1
        self.proc = None

    def start(self):
        shutil.rmtree(self.dd, ignore_errors=True)
        os.makedirs(REPLICA_ROOT, mode=0o700, exist_ok=True)
        subprocess.run(["cp", "-a", self.src, self.dd], check=True)
        for stale in ("regtest/.cookie", "regtest/.lock", "regtest/dashd.pid"):
            try:
                os.remove(os.path.join(self.dd, stale))
            except FileNotFoundError:
                pass
        self.args = chain_args(os.path.join(self.dd, "regtest", "debug.log"))
        cmd = [os.path.join(DASHD_DIR, "dashd"), f"-datadir={self.dd}", f"-rpcport={self.rpcport}",
               f"-port={self.port}", "-listen=1", "-listenonion=0", "-connect=0", "-rpcdoccheck=0",
               "-printtoconsole=0", "-checkblocks=1", "-checklevel=0", "-deprecatedrpc=service", *self.args]
        self.cmd = cmd
        self.proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        cookie = os.path.join(self.dd, "regtest", ".cookie")
        for _ in range(180):
            if self.proc.poll() is not None:
                raise RuntimeError(f"dashd exited: {self.proc.stderr.read().decode(errors='replace')}")
            try:
                self.rpc = Rpc("127.0.0.1", self.rpcport, cookie)
                self.rpc("getblockcount")
                return self.rpc
            except (OSError, RpcError, ValueError):
                time.sleep(1)
        self.stop(remove=False)
        raise RuntimeError(f"dashd in {self.dd} did not answer RPC")

    def stop(self, remove=True):
        if self.proc and self.proc.poll() is None:
            try:
                self.rpc("stop", timeout=30)
                self.proc.wait(120)
            except Exception:
                self.proc.send_signal(signal.SIGTERM)
                self.proc.wait(120)
        if remove:
            shutil.rmtree(self.dd, ignore_errors=True)


class RegtestWalker:
    def __init__(self, rpc, chain):
        self.rpc, self.chain = rpc, chain
        self.outputs = {}  # txid -> [scriptPubKey objects]; for external collateral lookups
        self.no_signer = {}
        self.quorum_cache = {}
        self.notes = []

    def quorum(self, quorum_hash, llmq_type):
        """Core's `quorum info` for the quorum of the type Core verifies the payload against.

        On regtest every LLMQ type forms a quorum at the same block, so one quorumHash names
        several quorums with different keys; the others are listed for transparency.
        """
        key = (quorum_hash, llmq_type)
        if key not in self.quorum_cache:
            chosen, others = None, []
            for number, name in sorted(LLMQ_TYPES.items()):
                try:
                    info = self.rpc("quorum", "info", number, quorum_hash)
                except RpcError:
                    continue
                if number == llmq_type:
                    chosen = {"llmq_type": number, "llmq_name": name,
                              "quorum_public_key": info["quorumPublicKey"], "quorum info": info}
                else:
                    others.append({"llmq_type": number, "llmq_name": name,
                                   "quorum_public_key": info["quorumPublicKey"]})
            if chosen is not None:
                chosen["other_types_with_same_quorum_hash"] = others
            self.quorum_cache[key] = chosen
        return self.quorum_cache[key]

    def mn_state(self, pro_tx_hash, parent_hash):
        try:
            return self.rpc("protx", "info", pro_tx_hash, parent_hash)
        except RpcError as e:
            return {"error": str(e)}

    def signer(self, ttype, payload, height, parent_hash):
        if ttype == 1:
            outpoint_hash = payload.get("collateralHash")
            if outpoint_hash and outpoint_hash != NULL_HASH:
                idx = payload.get("collateralIndex")
                outs = self.outputs.get(outpoint_hash)
                if outs is None:
                    try:
                        outs = self.rpc("getrawtransaction", outpoint_hash, True)["vout"]
                    except RpcError as e:
                        return {"kind": "external_collateral", "error": str(e)}
                spk = outs[idx]["scriptPubKey"]
                return {"kind": "external_collateral", "collateral_outpoint": f"{outpoint_hash}:{idx}",
                        "collateral_script": spk["hex"], "collateral_address": spk.get("address"),
                        "collateral_value": outs[idx].get("value")}
            if payload.get("shares"):
                return {"kind": "internal_collateral_with_shares", "note": "share owner keys are in the payload"}
            return {"kind": "internal_collateral", "note": "no payload signature"}
        if ttype in (2, 3, 4, 10, 11, 12):
            pro = payload.get("proTxHash")
            if not pro:
                return {"kind": "mn_state", "error": "payload has no proTxHash"}
            info = self.mn_state(pro, parent_hash)
            state = info.get("state", {}) if isinstance(info, dict) else {}
            out = {"kind": "mn_state", "proTxHash": pro, "as_of_block": parent_hash, "as_of_height": height - 1,
                   "mn_type": info.get("type"), "mn_version": state.get("version", info.get("nVersion")),
                   "state": info}
            if ttype in (2, 4, 10, 11, 12):
                out["operator_pubkey"] = state.get("pubKeyOperator")
            if ttype in (3, 10, 11, 12):
                out["owner_address"] = state.get("ownerAddress")
                out["share_owner_addresses"] = [sh.get("ownerAddress") for sh in state.get("shares", [])]
            return out
        if ttype in (7, 9):
            qh = payload.get("quorumHash") or (payload.get("signal") or {}).get("quorumHash")
            llmq_type = REGTEST_LLMQ_MNHF if ttype == 7 else REGTEST_LLMQ_PLATFORM
            quorum = self.quorum(qh, llmq_type) if qh else None
            out = {"kind": "quorum", "quorumHash": qh, "llmq_type_source": REGTEST_LLMQ_SOURCE, "quorum": quorum}
            if quorum is None:
                out["error"] = f"no llmq type {llmq_type} quorum with this hash"
            return out
        return None

    def keep(self, ttype, payload):
        if ttype in (5, 8) or (ttype == 6 and not commitment_has_signers(payload)):
            key = (ttype, payload.get("version"), (payload.get("commitment") or {}).get("version"))
            self.no_signer[key] = self.no_signer.get(key, 0) + 1
            return self.no_signer[key] <= NO_SIGNER_CAP
        return True

    def walk(self):
        entries, counts = [], {}
        tip = self.rpc("getblockcount")
        parent_hash = None
        for h in range(tip + 1):
            bh = self.rpc("getblockhash", h)
            block = self.rpc("getblock", bh, 2)
            for tx in block["tx"]:
                self.outputs[tx["txid"]] = tx["vout"]
                ttype = tx.get("type", 0)
                if tx.get("version") != 3 or not 1 <= ttype <= 12:
                    continue
                key, payload = payload_of(tx)
                counts[(ttype, (payload or {}).get("version"))] = counts.get((ttype, (payload or {}).get("version")), 0) + 1
                if payload is None:
                    self.notes.append(f"h={h} {tx['txid']}: type {ttype} without a decoded payload object")
                    payload = {}
                if not self.keep(ttype, payload):
                    continue
                entries.append({
                    "chain": self.chain, "height": h, "block_hash": bh, "txid": tx["txid"], "type": ttype,
                    "type_name": TYPE_NAMES.get(ttype), "payload_key": key,
                    "payload_version": payload.get("version"), "hex": tx["hex"],
                    "signer": self.signer(ttype, payload, h, parent_hash) if h > 0 else None,
                    "core": payload,
                })
            flag_same_block_mn_txs([e for e in entries if e["height"] == h], self.notes)
            parent_hash = bh
        return entries, counts


def flag_same_block_mn_txs(block_entries, notes):
    """Mark entries whose MN is touched by another special tx of the same block.

    The signer state is the state at h-1; an earlier tx of the same block may already have
    changed it by the time Core checked this one.
    """
    by_mn = {}
    for e in block_entries:
        if e["core"].get("proTxHash"):
            by_mn.setdefault(e["core"]["proTxHash"], []).append(e)
    for pro, group in by_mn.items():
        if len(group) > 1:
            for e in group:
                e["same_block_mn_txs"] = [o["txid"] for o in group if o is not e]
            notes.append(f"h={group[0]['height']}: {len(group)} txs for MN {pro} in one block "
                         f"(types {[e['type'] for e in group]}, block order); signer state is h-1")


def commitment_has_signers(qc_payload):
    commitment = qc_payload.get("commitment") or {}
    signers = commitment.get("signers", "")
    return any(c not in "0" for c in signers) if signers else False


def run_regtest(chains, out_dir):
    dashd = os.path.join(DASHD_DIR, "dashd")
    for index, chain in enumerate(chains):
        node = Node(chain, index)
        print(f"[{chain}] starting dashd on replica {node.dd}", file=sys.stderr)
        try:
            rpc = node.start()
            walker = RegtestWalker(rpc, chain)
            entries, counts = walker.walk()
            provenance = {
                "producer": "Dash Core dashd over JSON-RPC (gen_sigvec.py regtest)",
                "dashd_version": rpc("getnetworkinfo")["subversion"],
                "dashd_binary": dashd, "dashd_sha256": sha256_file(dashd),
                "release_tarball": TARBALL, "release_tarball_sha256": sha256_file(TARBALL),
                "source_datadir": node.src, "replica": node.dd,
                "chain_args": node.args, "dashd_command": node.cmd,
                "source_dash_conf": open(os.path.join(node.src, "dash.conf")).read().splitlines(),
                "date": now(),
                "signer_lookup": "as of the parent block (h-1): protx info <proTxHash> <hash(h-1)>; "
                                 "quorum info <type> <quorumHash> at tip",
                "caps": f"CbTx, AssetLockTx and null QcTx: at most {NO_SIGNER_CAP} per (type, payload version, "
                        "commitment version); non-null QcTx and all signed types: every one",
                "special_tx_counts_all": {f"{t}/v{v}": n for (t, v), n in sorted(counts.items(), key=str)},
                "getblockchaininfo": rpc("getblockchaininfo"),
                "getdeploymentinfo": rpc("getdeploymentinfo"),
                "quorum_list": rpc("quorum", "list"),
                "notes": walker.notes,
            }
            path = os.path.join(out_dir, f"regtest-{chain}.json")
            with open(path, "w") as f:
                json.dump({"provenance": provenance, "entries": entries}, f, indent=1)
            print(f"[{chain}] {len(entries)} entries -> {path}", file=sys.stderr)
        finally:
            node.stop()


# --------------------------------------------------------------------------------------- mainnet

DIP3_HEIGHT = 1028160
V19_HEIGHT = 1987776
MAINNET_WINDOW = 60          # consecutive blocks scanned per window
MAINNET_PER_WINDOW = 2       # samples of one kind taken from one window (spreads heights)
MAINNET_LLMQ_MNHF = 3        # mainnet consensus.llmqTypeMnhf = LLMQ_400_85 (v24 chainparams.cpp)


def mainnet_kind(tx, payload):
    """Sampling bucket of a mainnet special tx, or None when it is not sampled."""
    t, v = tx["type"], payload.get("version")
    if t in (2, 3, 4):
        return f"{TYPE_NAMES[t]}/v{v}"
    if t == 6:
        c = payload.get("commitment") or {}
        return f"QcTx/commitment-v{c.get('version')}" if commitment_has_signers(payload) else None
    if t == 7:
        return f"MnHfTx/v{v}"
    return None


def window_starts(lo, hi):
    """Window starts spread over [lo, hi): van der Corput order, so any prefix is spread out."""
    span, n = hi - lo - MAINNET_WINDOW, 1
    while True:
        x, denom, k = 0.0, 1.0, n
        while k:
            denom *= 2
            x += (k & 1) / denom
            k >>= 1
        yield lo + int(x * span)
        n += 1


class MainnetSampler:
    def __init__(self, rpc, per_kind, max_blocks):
        self.rpc, self.per_kind, self.max_blocks = rpc, per_kind, max_blocks
        self.lists = {}   # height -> {proTxHash: MN json} from `protx listdiff 1 <height>`
        self.notes = []
        self.scanned = 0
        self.windows = []

    def mn_list(self, height):
        if height not in self.lists:
            diff = self.rpc("protx", "listdiff", 1, height)
            self.lists = {height: {mn["proTxHash"]: mn for mn in diff["addedMNs"]}}  # keep one list in memory
        return self.lists[height]

    def scan(self):
        tip = self.rpc("getblockchaininfo")["blocks"]
        eras = [(DIP3_HEIGHT, V19_HEIGHT), (V19_HEIGHT, tip + 1)]
        gens = [window_starts(lo, hi) for lo, hi in eras]
        found, counts, scanned_heights = [], {}, set()
        era = 0
        while self.scanned < self.max_blocks:
            start = next(gens[era])
            era ^= 1
            taken = {}
            heights = [h for h in range(start, start + MAINNET_WINDOW) if h not in scanned_heights]
            self.windows.append([start, start + MAINNET_WINDOW - 1])
            for h in heights:
                scanned_heights.add(h)
                self.scanned += 1
                bh = self.rpc("getblockhash", h)
                block = self.rpc("getblock", bh, 2)
                for tx in block["tx"]:
                    if tx.get("version") != 3 or not 1 <= tx.get("type", 0) <= 12:
                        continue
                    key, payload = payload_of(tx)
                    kind = mainnet_kind(tx, payload or {})
                    if kind is None or counts.get(kind, 0) >= self.per_kind or taken.get(kind, 0) >= MAINNET_PER_WINDOW:
                        continue
                    counts[kind] = counts.get(kind, 0) + 1
                    taken[kind] = taken.get(kind, 0) + 1
                    found.append((kind, h, bh, block["previousblockhash"], tx, key, payload))
            print(f"[mainnet] scanned {self.scanned} blocks; {dict(sorted(counts.items()))}", file=sys.stderr)
            wanted = ["ProUpServTx/v1", "ProUpServTx/v2", "ProUpRevTx/v1", "ProUpRevTx/v2", "ProUpRegTx/v1",
                      "ProUpRegTx/v2"]
            if all(counts.get(k, 0) >= self.per_kind for k in wanted):
                break
        return found, counts, tip

    def signer(self, kind, h, parent_hash, tx, payload):
        t = tx["type"]
        if t in (2, 3, 4):
            pro = payload["proTxHash"]
            mn = self.mn_list(h - 1).get(pro)
            if mn is None:
                return {"kind": "mn_state", "proTxHash": pro, "error": f"not in protx listdiff 1 {h - 1}"}
            out = {"kind": "mn_state", "proTxHash": pro, "as_of_block": parent_hash, "as_of_height": h - 1,
                   "source_rpc": f"protx listdiff 1 {h - 1}", "mn_type": mn.get("type"),
                   "mn_version": mn["state"].get("version"), "state": mn}
            if t in (2, 4):
                out["operator_pubkey"] = mn["state"].get("pubKeyOperator")
            else:
                out["owner_address"] = mn["state"].get("ownerAddress")
            return out
        if t == 7:
            return self.mnhf_quorum(payload)
        return None

    def mnhf_quorum(self, payload):
        """Quorum key of an MnHf signal from the QcTx that mined that quorum (scan ~60 blocks)."""
        qh = (payload.get("signal") or {}).get("quorumHash")
        out = {"kind": "quorum", "quorumHash": qh, "llmq_type_source": "mainnet llmqTypeMnhf = LLMQ_400_85"}
        try:
            qheight = self.rpc("getblockheader", qh)["height"]
        except RpcError as e:
            out["error"] = str(e)
            return out
        for h in range(qheight, qheight + 60):
            for tx in self.rpc("getblock", self.rpc("getblockhash", h), 2)["tx"]:
                c = (tx.get("qcTx") or {}).get("commitment") or {}
                if c.get("quorumHash") == qh and c.get("llmqType") == MAINNET_LLMQ_MNHF:
                    out["quorum"] = {"llmq_type": MAINNET_LLMQ_MNHF, "quorum_public_key": c["quorumPublicKey"],
                                     "mined_height": h, "qc_txid": tx["txid"], "commitment": c}
                    return out
        out["error"] = "commitment not found within 60 blocks"
        return out


def run_mainnet(per_kind, max_blocks, out_dir):
    rpc = Rpc(MAINNET["host"], MAINNET["port"], MAINNET["cookie"], allowed=MAINNET_ALLOWED)
    sampler = MainnetSampler(rpc, per_kind, max_blocks)
    found, counts, tip = sampler.scan()
    entries = []
    for kind, h, bh, parent, tx, key, payload in sorted(found, key=lambda f: f[1]):
        entries.append({
            "chain": "mainnet", "height": h, "block_hash": bh, "txid": tx["txid"], "type": tx["type"],
            "type_name": TYPE_NAMES.get(tx["type"]), "sample_kind": kind, "payload_key": key,
            "payload_version": payload.get("version"), "hex": tx["hex"],
            "signer": sampler.signer(kind, h, parent, tx, payload), "core": payload,
        })
    chaininfo = rpc("getblockchaininfo")
    provenance = {
        "producer": "Dash Core mainnet reference node over JSON-RPC (gen_sigvec.py mainnet)",
        "node": f"{MAINNET['host']}:{MAINNET['port']} (still syncing; read-only RPC subset)",
        "rpcs_used": sorted(MAINNET_ALLOWED), "date": now(), "tip_at_scan": tip,
        "getblockchaininfo": chaininfo,
        "sampling": {"eras": [[DIP3_HEIGHT, V19_HEIGHT - 1], [V19_HEIGHT, tip]], "window": MAINNET_WINDOW,
                     "per_kind": per_kind, "per_window_per_kind": MAINNET_PER_WINDOW,
                     "blocks_scanned": sampler.scanned, "windows": sampler.windows, "sampled_counts": counts},
        "signer_lookup": "MN state as of h-1 from `protx listdiff 1 <h-1>` (one call per block with samples); "
                         "external collateral script from getrawtransaction (60 s timeout); MnHf quorum key from "
                         "the QcTx that mined the signing quorum",
        "notes": sampler.notes,
    }
    path = os.path.join(out_dir, "mainnet.json")
    with open(path, "w") as f:
        json.dump({"provenance": provenance, "entries": entries}, f, indent=1)
    print(f"[mainnet] {len(entries)} entries -> {path}", file=sys.stderr)


# --------------------------------------------------------------------------------------- summary

def signer_status(entry):
    s = entry["signer"]
    if entry["type"] == 6:
        return "in payload" if commitment_has_signers(entry["core"]) else "null commitment"
    if s is None:
        return "none (unsigned)"
    if "error" in s or (isinstance(s.get("state"), dict) and "error" in s["state"]):
        return "ERROR"
    if s["kind"] in ("internal_collateral", "internal_collateral_with_shares"):
        return s["kind"].replace("_", " ")
    if s["kind"] == "external_collateral":
        return "collateral script" if s.get("collateral_script") else "MISSING"
    if s["kind"] == "quorum":
        return f"quorum key (llmq {s['quorum']['llmq_type']})" if s.get("quorum") else "MISSING"
    if s["kind"] == "mn_state":
        if entry["type"] in (10, 11, 12):
            return f"MN state with {len(s['share_owner_addresses'])} share owners (state v{s.get('mn_version')}, {s.get('mn_type')})"
        if entry["type"] in (2, 4):
            return f"operator key (state v{s.get('mn_version')}, {s.get('mn_type')})"
        return f"owner key (state v{s.get('mn_version')}, {s.get('mn_type')})"
    return s["kind"]


def subversion(entry):
    """Version inside the payload that also selects the signing scheme (QcTx commitment version)."""
    if entry["type"] == 6:
        return f"c{(entry['core'].get('commitment') or {}).get('version')}"
    return ""


def write_summary(out_dir):
    files = sorted(f for f in os.listdir(out_dir) if f.endswith(".json"))
    lines = ["# BS6b signed special-tx fixtures (generated by `gen_sigvec.py summary`)", "",
             "Counts of fixture entries per (chain, type, payload version, signer info). `all in chain` is "
             "the number of txs of that type/version on the chain (regtest) before the caps.", ""]
    for name in files:
        doc = json.load(open(os.path.join(out_dir, name)))
        prov = doc["provenance"]
        totals = prov.get("special_tx_counts_all", {})
        counts = {}
        for e in doc["entries"]:
            key = (e["type"], e["type_name"], e["payload_version"], subversion(e), signer_status(e))
            counts[key] = counts.get(key, 0) + 1
        lines += [f"## {name}", ""]
        if "dashd_version" in prov:
            lines += [f"- dashd {prov['dashd_version']} (sha256 {prov['dashd_sha256']}), source "
                      f"`{prov['source_datadir']}`, chain args `{' '.join(prov['chain_args'])}`, "
                      f"tip {prov['getblockchaininfo']['blocks']}", ""]
        else:
            s = prov["sampling"]
            lines += [f"- tip {prov['tip_at_scan']}, {s['blocks_scanned']} blocks scanned in "
                      f"{len(s['windows'])} windows of {s['window']}", ""]
        lines += ["| type | name | payload v | sub | signer info | entries | all in chain |",
                  "|---:|---|---:|---|---|---:|---:|"]
        for (t, tn, v, sub, st), n in sorted(counts.items(), key=lambda kv: tuple(map(str, kv[0]))):
            lines.append(f"| {t} | {tn} | {v} | {sub} | {st} | {n} | {totals.get(f'{t}/v{v}', '')} |")
        if prov.get("notes"):
            lines += ["", "Notes:"] + [f"- {n}" for n in prov["notes"]]
        lines.append("")
    with open(os.path.join(out_dir, "COUNTS.md"), "w") as f:
        f.write("\n".join(lines))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("regtest")
    r.add_argument("--chains", default=",".join(CHAINS))
    r.add_argument("--out", default=HERE)
    m = sub.add_parser("mainnet")
    m.add_argument("--per-kind", type=int, default=25)
    m.add_argument("--max-blocks", type=int, default=4000, help="block scan budget (~0.3 s per getblock 2)")
    m.add_argument("--out", default=HERE)
    s = sub.add_parser("summary", help="write COUNTS.md (counts per chain/type/version/signer)")
    s.add_argument("--out", default=HERE)
    args = ap.parse_args()
    if args.cmd == "regtest":
        run_regtest(args.chains.split(","), args.out)
    elif args.cmd == "summary":
        write_summary(args.out)
    else:
        run_mainnet(args.per_kind, args.max_blocks, args.out)


if __name__ == "__main__":
    main()
