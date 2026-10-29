#!/usr/bin/env python3
"""Capture the regtest chain Dash Core's functional test feature_mnehf builds, and Core's
verdicts on crafted MNHF (EHF signal) blocks (node-program O2a-MNHF,
tests/data/mnhf-regtest-<pin>.json.gz).

The chain comes from `feature_mnehf.py` (tag v24.0.0-rc.3) run unchanged through the read-only
wrapper `feature_mnehf_capture.py` (see PROVENANCE.md), with `--nocleanup`. This script
restarts node0 alone on a copy of the test's datadir with the chain arguments the test's last
start of node0 logged (read from its debug.log), `-connect=0`, no listening, its own RPC port,
`-rpcdoccheck=0` and `-checkblocks=0`, and reads everything over RPC:
- `headers`, `blocks` (raw block, MNHF transactions from `getblock 2`), `deployments`
  (`getdeploymentinfo` of every height, reduced to the BIP9 entries) and the blocks that are
  not on the active chain (`getchaintips`);
- `mempool_at_capture`: the wrapper's snapshots of node0's mempool (the duplicate signal
  exists only there, the test restarts with -persistmempool=0);
- `proposals`: Core's answer (`getblocktemplate` mode proposal, null = accepted) for blocks
  built from the chain's own blocks by appending MNHF transactions. Each base is reached with
  `invalidateblock` and the tip restored with `reconsiderblock` afterwards (checked). The
  proposal check does not look at proof of work, so the merkle root is recomputed and the
  header hash stays whatever it is.

Usage: gen-mnhf-regtest.py --dashd <release>/bin/dashd --pin v24.0 --node0 <ft>/feature_mnehf/node0 \\
           --capture <work>/mempool-capture.json --wrapper <...>/feature_mnehf_capture.py \\
           --test-file <...>/feature_mnehf.py --datadir <scratch>/dd --port 29931 \\
           --out tests/data/mnhf-regtest-v24.0.json.gz
"""

import argparse
import base64
import datetime
import gzip
import hashlib
import importlib.util
import json
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
TAG = "v24.0.0-rc.3"
TEST = "feature_mnehf"
MNHF_TYPE = 7


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


regtest = load("gen_contextual_regtest", "gen-contextual-regtest.py")
split_block, join_block, dsha256 = regtest.split_block, regtest.join_block, regtest.dsha256

# Arguments of the test's last node0 start that change what the node does but not the chain's
# rules: paths, ports, logging, performance, the mock clock, mempool persistence, RPC checks.
IGNORED_ARGS = {
    "datadir", "debug", "debugexclude", "loglevel", "logsourcelocations", "logthreadnames",
    "logtimemicros", "maxsigcachesize", "mocktime", "par", "parbls", "persistmempool",
    "rpcthreads", "uacomment", "v2transport", "createwalletbackups", "regtest", "bind",
    "discover", "dnsseed", "fallbackfee", "fixedseeds", "keypool", "listenonion", "natpmp",
    "peertimeout", "port", "printtoconsole", "rpcdoccheck", "rpcport", "rpcservertimeout",
    "server", "shrinkdebugfile", "sporkkey", "tinyblk", "unsafesqlitesync", "upnp",
}
CHAIN_ARGS = {"vbparams", "dip3params", "testactivationheight", "llmqtestparams",
              "llmqtestinstantsendparams", "llmqtestplatformparams"}
# Lines of the node's datadir config that would clash with the restart's command line.
CONF_DROP = re.compile(r"^(bind|port|rpcport|rpcdoccheck|listen|connect)=")


def sha256_file(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def last_start_chain_args(debug_log):
    """The chain arguments of the last start in node0's debug.log, in log order."""
    lines = Path(debug_log).read_text(errors="replace").splitlines()
    starts = [i for i, line in enumerate(lines) if "[LogPackageVersion] Dash Core version" in line]
    args = []
    for line in lines[starts[-1]:]:
        m = re.search(r"(Command-line arg|Config file arg): (?:\[regtest\] )?(\w+)=\"(.*)\"$", line)
        if not m:
            continue
        name, value = m.group(2), m.group(3)
        if name in IGNORED_ARGS:
            continue
        if name not in CHAIN_ARGS:
            raise RuntimeError(f"unclassified node0 argument: {name}={value}")
        args.append(f"-{name}={value}")
    return args


class Node:
    def __init__(self, dashd, datadir, port, chain_args):
        self.datadir = Path(datadir)
        self.rpc_port = port + 1
        args = [dashd, "-regtest", f"-datadir={self.datadir}", f"-port={port}",
                f"-rpcport={self.rpc_port}", "-rpcuser=o2a", "-rpcpassword=o2a", "-listen=0",
                "-connect=0", "-listenonion=0", "-rpcdoccheck=0", "-checkblocks=0",
                "-persistmempool=0", "-printtoconsole=0", "-par=2", "-parbls=2",
                "-rpcthreads=4", *chain_args]
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.auth = base64.b64encode(b"o2a:o2a").decode()
        for _ in range(300):
            if self.proc.poll() is not None:
                raise RuntimeError(f"dashd exited with {self.proc.returncode}")
            try:
                self.call("getblockcount")
                return
            except Exception:
                time.sleep(1)
        raise RuntimeError("dashd did not start")

    def call(self, method, *params):
        url = f"http://127.0.0.1:{self.rpc_port}/"
        body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
        req = urllib.request.Request(url, data=body, headers={"Authorization": f"Basic {self.auth}"})
        try:
            with urllib.request.urlopen(req, timeout=600) as resp:
                reply = json.load(resp)
        except urllib.error.HTTPError as e:
            reply = json.load(e)
        if reply.get("error"):
            raise RuntimeError(f"{method}{params}: {reply['error']}")
        return reply["result"]

    def stop(self):
        try:
            self.call("stop")
        except Exception:
            pass
        self.proc.wait(timeout=300)


def mnhf_of(block_json):
    """The MNHF transactions of a `getblock 2` result."""
    out = []
    for index, tx in enumerate(block_json["tx"]):
        if tx.get("type") != MNHF_TYPE and "mnhfTx" not in tx:
            continue
        signal = tx["mnhfTx"]["signal"]
        out.append({"txid": tx["txid"], "index_in_block": index,
                    "versionBit": signal["versionBit"], "quorumHash": signal["quorumHash"],
                    "sig": signal["sig"]})
    return out


def reduce_deployments(info):
    out = {}
    for name, dep in sorted(info["deployments"].items()):
        if dep.get("type") != "bip9":
            continue
        bip9 = dep["bip9"]
        out[name] = {"bit": bip9.get("bit"), "ehf": bip9.get("ehf"),
                     "ehf_height": bip9.get("ehf_height"), "status": bip9["status"],
                     "status_next": bip9["status_next"]}
    return out


def copy_datadir(src, dst):
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(src, dst, symlinks=True)
    dst = Path(dst)
    for lock in (".lock", ".cookie"):
        (dst / "regtest" / lock).unlink(missing_ok=True)
    conf = dst / "dash.conf"
    conf.write_text("\n".join(l for l in conf.read_text().splitlines() if not CONF_DROP.match(l)) + "\n")
    dst.chmod(0o700)


def tx_hex_with_payload_replaced(tx_hex, quorum_hash, zero_sig, new_quorum_hash=None):
    """A copy of an MNHF transaction with another quorumHash and/or an all-zero signature.
    The payload ends with quorumHash (32 bytes, internal order) and the 96-byte signature."""
    raw = bytearray(bytes.fromhex(tx_hex))
    sig_start = len(raw) - 96
    q_start = sig_start - 32
    assert bytes(raw[q_start:sig_start]) == bytes.fromhex(quorum_hash)[::-1], "payload layout"
    if new_quorum_hash is not None:
        raw[q_start:sig_start] = bytes.fromhex(new_quorum_hash)[::-1]
    if zero_sig:
        raw[sig_start:] = bytes(96)
    return bytes(raw).hex()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--pin", default="v24.0")
    ap.add_argument("--node0", required=True, help="node0's datadir the functional test left")
    ap.add_argument("--capture", required=True, help="the wrapper's mempool capture JSON")
    ap.add_argument("--wrapper", required=True, help="feature_mnehf_capture.py")
    ap.add_argument("--test-file", required=True, help="feature_mnehf.py of the tag")
    ap.add_argument("--datadir", required=True)
    ap.add_argument("--port", type=int, default=29931)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    dashd = str(Path(args.dashd).resolve())
    version = subprocess.run([dashd, "-version"], capture_output=True, text=True, check=True).stdout.splitlines()[0]
    chain_args = last_start_chain_args(Path(args.node0) / "regtest" / "debug.log")
    captures = json.loads(Path(args.capture).read_text())
    copy_datadir(args.node0, args.datadir)
    node = Node(dashd, args.datadir, args.port, chain_args)
    try:
        out = capture(node, captures, args, version, chain_args)
    finally:
        node.stop()
        shutil.rmtree(args.datadir, ignore_errors=True)

    data = json.dumps(out, sort_keys=True, separators=(",", ":")).encode()
    with open(args.out, "wb") as f:
        with gzip.GzipFile(filename="", mode="wb", fileobj=f, mtime=0, compresslevel=9) as gz:
            gz.write(data)
    print(f"wrote {args.out}: {Path(args.out).stat().st_size} bytes, {len(out['blocks'])} blocks", file=sys.stderr)


def capture(node, captures, args, version, chain_args):
    tip_height = node.call("getblockcount")
    tip_hash = node.call("getbestblockhash")
    hashes = [node.call("getblockhash", h) for h in range(tip_height + 1)]
    assert hashes[-1] == tip_hash
    tip_time = node.call("getblockheader", tip_hash)["time"]
    node.call("setmocktime", tip_time + 150)

    headers = [node.call("getblockheader", h, False) for h in hashes]
    blocks, deployments = [], []
    for height, h in enumerate(hashes):
        deployments.append(reduce_deployments(node.call("getdeploymentinfo", h)))
        if height == 0:
            continue
        blocks.append({"height": height, "hash": h, "hex": node.call("getblock", h, 0),
                       "mnhf": mnhf_of(node.call("getblock", h, 2))})
    dep_names = sorted({name for d in deployments for name in d})

    # Blocks that are not on the active chain, one record per block of each side branch.
    invalidated = []
    for t in node.call("getchaintips"):
        if t["status"] == "active":
            continue
        branch = []
        h = t["hash"]
        while node.call("getblockheader", h)["confirmations"] == -1:
            branch.append(h)
            h = node.call("getblockheader", h)["previousblockhash"]
        for bh in reversed(branch):
            hdr = node.call("getblockheader", bh)
            invalidated.append({
                "hash": bh, "height": hdr["height"], "previousblockhash": hdr["previousblockhash"],
                "branch_tip": t["hash"], "branch_tip_status": t["status"],
                "branchlen": t["branchlen"], "forkpoint": t["forkpoint"],
                "hex": node.call("getblock", bh, 0), "mnhf": mnhf_of(node.call("getblock", bh, 2))})

    def spawn():
        """A second private node started from a fresh copy of node0's datadir, on which the
        bases below a side branch are judged (see go_to)."""
        dd = Path(str(args.datadir) + "-fresh")
        copy_datadir(args.node0, dd)
        fresh = Node(str(Path(args.dashd).resolve()), dd, args.port + 10, chain_args)
        fresh.call("setmocktime", tip_time + 150)
        fresh.datadir = dd
        return fresh

    proposals = make_proposals(node, spawn, hashes, blocks, invalidated, captures)
    assert node.call("getbestblockhash") == tip_hash, "tip not restored"

    return {
        "provenance": {
            "dashd_version": version,
            "dashd_sha256": sha256_file(args.dashd),
            "functional_test": TEST,
            "core_tag": TAG,
            "test_file_sha256": sha256_file(args.test_file),
            "capture_wrapper": Path(args.wrapper).read_text(),
            "capture_date_utc": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "generator": "dash-oracle-sys/scripts/gen-mnhf-regtest.py",
            "node0_chain_args": chain_args,
            "restart_args": ["-connect=0", "-listen=0", "-rpcdoccheck=0", "-checkblocks=0", "-persistmempool=0"],
            "tip_height": tip_height,
            "tip_hash": tip_hash,
            "adjusted_time": tip_time + 150,
            "bip9_deployment_names": dep_names,
        },
        "chain": "regtest",
        "chain_args": chain_args,
        "headers": headers,
        "blocks": blocks,
        "deployments": deployments,
        "invalidated": invalidated,
        "mempool_at_capture": captures,
        "proposals": proposals,
    }


def go_to(node, hashes, side_blocks, base_height):
    """Make the active block at base_height the tip. Returns the hashes invalidated, to be
    reconsidered by the caller. The side-branch blocks on top of the base go first, so that the
    side branch does not take over. Bases below a side branch are judged on a second node
    started from a fresh copy of the datadir (see make_proposals)."""
    done = []
    if node.call("getblockcount") == base_height:
        return done
    for b in side_blocks:
        if b["previousblockhash"] == hashes[base_height]:
            node.call("invalidateblock", b["hash"])
            done.append(b["hash"])
    node.call("invalidateblock", hashes[base_height + 1])
    done.append(hashes[base_height + 1])
    assert node.call("getblockcount") == base_height, "could not reach the base"
    assert node.call("getbestblockhash") == hashes[base_height]
    return done


def make_proposals(node, spawn, hashes, blocks, invalidated, captures):
    by_height = {b["height"]: b for b in blocks}
    tip = len(hashes) - 1

    # The duplicate same-bit signal the test left in node0's mempool: the last snapshot that
    # holds a bit 28 signal (taken right after the test sent it).
    snap = next(s for s in reversed(captures)
                if any(t["decoded_mnhfTx"]["signal"]["versionBit"] == 28 for t in s["txs"]))
    dup = next(t for t in snap["txs"] if t["decoded_mnhfTx"]["signal"]["versionBit"] == 28)
    dup_hex, dup_txid = dup["hex"], dup["txid"]
    dup_quorum = dup["decoded_mnhfTx"]["signal"]["quorumHash"]
    signal_heights = [b["height"] for b in blocks if any(m["txid"] == dup_txid for m in b["mnhf"])]
    # A second quorum of the signing type (llmq_test) the chain mined, never used by a signal.
    other_quorum = next((q for q in node.call("quorum", "list")["llmq_test"] if q != dup_quorum), None)

    side_top = max(b["height"] for b in invalidated)

    def propose(name, base_height, note, add=()):
        """The active chain's block at base_height + 1 with the transactions of `add`
        ((label, hex) pairs) appended and the merkle root recomputed, judged on top of base_height.
        Bases below a side branch run on a fresh node that is discarded afterwards."""
        source = by_height[base_height + 1]
        raw = bytes.fromhex(source["hex"])
        if add:
            header, txs = split_block(raw)
            raw = join_block(header, txs + [bytes.fromhex(h) for _, h in add])
        else:
            # The block as mined was invalidated to reach the base, so Core would answer
            # duplicate-invalid; flip the nonce (proof of work is not checked) to get a new hash.
            nonce = int.from_bytes(raw[76:80], "little") ^ 0x5A5A5A5A
            raw = raw[:76] + nonce.to_bytes(4, "little") + raw[80:]
        request = {"mode": "proposal", "data": raw.hex()}
        if base_height < side_top:
            n = spawn()
            try:
                go_to(n, hashes, invalidated, base_height)
                result = n.call("getblocktemplate", request)
            finally:
                n.stop()
                shutil.rmtree(n.datadir, ignore_errors=True)
        else:
            undo = go_to(node, hashes, invalidated, base_height)
            try:
                result = node.call("getblocktemplate", request)
            finally:
                for h in reversed(undo):
                    node.call("reconsiderblock", h)
            assert node.call("getbestblockhash") == hashes[tip], "tip not restored"
        return {
            "name": name, "note": note, "base_height": base_height, "base_hash": hashes[base_height],
            "source_block_height": base_height + 1, "source_block_hash": hashes[base_height + 1],
            "added": [{"label": label, "txid": dsha256(bytes.fromhex(h))[::-1].hex(), "hex": h}
                      for label, h in add],
            "header_nonce_flipped": not add, "block_hex": raw.hex(), "core_result": result,
        }

    out = [
        propose("control_tip_block", tip - 1,
                "the active tip block, unchanged but for its nonce, on its parent (expect accepted)"),
        propose("tip_block_plus_duplicate_same_bit", tip - 1,
                "the tip block with the mempool duplicate of bit 28 appended; the signal is in "
                f"force at its parent ({deployment_in_force(node, hashes, tip - 1, 'testdummy')})",
                add=[("mempool duplicate of bit 28 (same bytes as the mined signal)", dup_hex)]),
    ]
    for h in signal_heights:
        out.append(propose(f"control_signal_block_{h}", h - 1,
                           f"the active block {h}, which mines the bit 28 signal, unchanged but for its "
                           "nonce (expect accepted)"))
    last = signal_heights[-1]
    out.append(propose(f"signal_block_{last}_plus_same_tx_again", last - 1,
                       f"block {last} with its own bit 28 signal transaction appended a second time",
                       add=[("same bytes as the block's own signal", dup_hex)]))
    # Core signs a request id once, so no second valid bit 28 signal exists; these two are crafted.
    out.append(propose(f"signal_block_{last}_plus_bit28_zero_signature", last - 1,
                       f"block {last} with a second bit 28 signal appended: the same quorum, an "
                       "all-zero signature (crafted)",
                       add=[("bit 28, quorum of the original signal, zero signature (crafted)",
                             tx_hex_with_payload_replaced(dup_hex, dup_quorum, True))]))
    if other_quorum:
        out.append(propose(f"signal_block_{last}_plus_bit28_other_quorum_zero_signature", last - 1,
                           f"block {last} with a second bit 28 signal appended: another mined "
                           "quorum, an all-zero signature (crafted)",
                           add=[("bit 28, other quorum, zero signature (crafted)",
                                 tx_hex_with_payload_replaced(dup_hex, dup_quorum, True, other_quorum))]))
    return out


def deployment_in_force(node, hashes, height, name):
    info = node.call("getdeploymentinfo", hashes[height])["deployments"][name]["bip9"]
    return f"{name} {info['status']}, ehf_height {info.get('ehf_height')}"


if __name__ == "__main__":
    main()
