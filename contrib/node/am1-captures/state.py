#!/usr/bin/env python3
"""Core parity: active/masternode.cpp:112-276 at v24.0.0."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[3]


def wait(check, label, timeout=180):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            result = check()
            if result:
                return result
        except (OSError, RuntimeError):
            pass
        time.sleep(0.1)
    raise RuntimeError(f"timeout: {label}")


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class Node:
    def __init__(self, binary, source, work, extra, wallet=""):
        self.dir, self.binary = work, binary
        shutil.copytree(source, work)
        work.chmod(0o700)
        (work / "dash.conf").write_text("")
        for name in ["peers.dat", "anchors.dat", "debug.log", "mempool.dat"]:
            (work / "regtest" / name).unlink(missing_ok=True)
        self.p2p, self.rpcport = port(), port()
        self.wallet, self.offset = wallet, 0
        self.args = [str(binary), f"-datadir={work}", "-regtest", "-server", "-listen=1",
                     f"-port={self.p2p}", f"-bind=127.0.0.1:{self.p2p}",
                     f"-externalip=127.0.0.1:{self.p2p}", f"-rpcport={self.rpcport}",
                     "-rpcuser=am1", "-rpcpassword=am1", "-rpcbind=127.0.0.1",
                     "-rpcallowip=127.0.0.1", "-connect=0", "-listenonion=0", "-discover=0",
                     "-dnsseed=0", "-fixedseeds=0", "-upnp=0", "-natpmp=0", "-par=1",
                     "-parbls=1", "-fallbackfee=0.00001", "-mocktime=1700000063",
                     "-debug=net", "-printtoconsole=0", "-v2transport=0", "-logtimestamps=0", "-logthreadnames=0", *extra]
        if wallet:
            self.args.append(f"-wallet={wallet}")
        self.proc = None
        try:
            self.start()
        except BaseException:
            self.stop()
            raise

    def start(self):
        self.stderr = open(self.dir / "stderr.txt", "ab")
        self.proc = subprocess.Popen(self.args, stdout=self.stderr, stderr=self.stderr)
        def ready():
            if self.proc.poll() is not None:
                raise ValueError((self.dir / "stderr.txt").read_text())
            return self.rpc("getblockcount")
        wait(ready, "node startup")

    def stop(self):
        if self.proc and self.proc.poll() is None:
            try:
                self.rpc("stop")
                self.proc.wait(timeout=60)
            except (OSError, RuntimeError, subprocess.TimeoutExpired):
                pass
            finally:
                if self.proc.poll() is None:
                    self.proc.kill()
                    self.proc.wait()
        if hasattr(self, "stderr"):
            self.stderr.close()

    def rpc(self, method, *params):
        url = f"http://127.0.0.1:{self.rpcport}/"
        if self.wallet:
            url += f"wallet/{self.wallet}"
        request = urllib.request.Request(url, data=json.dumps({"jsonrpc": "1.0", "id": 1,
                     "method": method, "params": params}).encode(),
                     headers={"Authorization": "Basic " + base64.b64encode(b"am1:am1").decode()})
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                result = json.load(response)
        except urllib.error.HTTPError as error:
            result = json.load(error)
        if result["error"]:
            raise RuntimeError(f"{method}: {result['error']}")
        return result["result"]

    def logs(self):
        text = (self.dir / "regtest/debug.log").read_text(errors="replace")
        tail = text[self.offset:]
        self.offset = len(text)
        return [line for line in tail.splitlines() if "CActiveMasternodeManager::" in line]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--network", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--pose-rounds", type=int, default=6)
    parser.add_argument("--out", type=Path, default=ROOT / "dashd-masternode/tests/fixtures/am1-state/core-v24.json")
    args = parser.parse_args()
    args.work.mkdir(mode=0o700, parents=True, exist_ok=True)
    version = subprocess.check_output([args.binary, "-version"], text=True).splitlines()[0]
    if version != "Dash Core version v24.0.0":
        raise RuntimeError(version)
    metadata = json.loads((args.network / "network.json").read_text())
    nodes, rows, missing, offline_blocks = [], [], [], []
    try:
        controller = Node(args.binary, args.network / metadata["controller"]["datadir"],
                          args.work / "controller", [*metadata["dashd_extra_args"], f"-sporkkey={metadata['spork_private_key']}"], metadata["controller"]["wallet"])
        nodes.append(controller)
        mn_meta = metadata["masternodes"][0]
        mn = Node(args.binary, args.network / mn_meta["datadir"], args.work / "mn",
                  [*metadata["dashd_extra_args"], f"-masternodeblsprivkey={mn_meta['bls_private_key']}"])
        nodes.append(mn)
        mn.rpc("addnode", f"127.0.0.1:{controller.p2p}", "onetry")
        mine_to = controller.rpc("getnewaddress")
        pro = mn_meta["pro_tx_hash"]
        key = mn_meta["bls_private_key"]
        service = f"127.0.0.1:{mn.p2p}"
        def capture(label):
            height = controller.rpc("getblockcount")
            wait(lambda: mn.rpc("getblockcount") == height, "block sync")
            time.sleep(0.3)
            row = {"step": label, "height": height, "block": controller.rpc("getbestblockhash"),
                   "status": mn.rpc("masternode", "status"),
                   "list": controller.rpc("protx", "list", "registered", True),
                   "info": controller.rpc("protx", "info", pro), "logs": mn.logs(),
                   "local_service": service, "operator_public": mn_meta["bls_public_key"]}
            rows.append(row)
            print(label, row["status"]["state"], flush=True)
        def mine():
            controller.rpc("generatetoaddress", 1, mine_to)
        capture("startup")
        controller.rpc("protx", "update_service", pro, service, key)
        mine()
        capture("registered")
        controller.rpc("protx", "update_service", pro, f"127.0.0.2:{mn.p2p}", key)
        mine()
        capture("proupserv")
        controller.rpc("protx", "update_service", pro, service, key)
        mine()
        capture("service_restored")
        new_key = controller.rpc("bls", "fromsecret", "01" * 32)
        controller.rpc("protx", "update_registrar", pro, new_key["public"], "", "")
        mine()
        capture("proupreg")
        controller.rpc("protx", "update_registrar", pro, mn_meta["bls_public_key"], "", "")
        mine()
        capture("operator_key_restored")
        controller.rpc("protx", "update_service", pro, service, key)
        mine()
        capture("operator_restored")
        controller.rpc("protx", "revoke", pro, key, 1)
        mine()
        capture("prouprev")
        info = controller.rpc("protx", "info", pro)
        pro = controller.rpc("protx", "register", info["collateralHash"], info["collateralIndex"], service,
                             controller.rpc("getnewaddress"), mn_meta["bls_public_key"],
                             controller.rpc("getnewaddress"), 0, controller.rpc("getnewaddress"),
                             controller.rpc("listunspent")[0]["address"])
        mine()
        capture("reregister")
        info = controller.rpc("protx", "info", pro)
        try:
            replacement = controller.rpc("protx", "register", info["collateralHash"], info["collateralIndex"], service,
                                 controller.rpc("getnewaddress"), mn_meta["bls_public_key"],
                                 controller.rpc("getnewaddress"), 0, controller.rpc("getnewaddress"),
                                 controller.rpc("listunspent")[0]["address"])
            pro = replacement
            mine()
            capture("same_block_replacement")
        except RuntimeError as error:
            if "bad-protx-dup-key" not in str(error):
                raise
            missing.append({"step": "same_block_replacement", "reason": str(error)})
        others = []
        for peer_meta in metadata["masternodes"][1:]:
            peer = Node(args.binary, args.network / peer_meta["datadir"], args.work / peer_meta["datadir"],
                        [*metadata["dashd_extra_args"], f"-masternodeblsprivkey={peer_meta['bls_private_key']}"])
            nodes.append(peer)
            others.append(peer)
            peer.rpc("addnode", f"127.0.0.1:{controller.p2p}", "onetry")
            controller.rpc("protx", "update_service", peer_meta["pro_tx_hash"], f"127.0.0.1:{peer.p2p}", peer_meta["bls_private_key"])
        mine()
        capture("quorum_services")
        height = controller.rpc("getblockcount")
        for peer in others:
            wait(lambda: peer.rpc("getblockcount") == height, "quorum peer sync")
        for peer in [controller, mn, *others]:
            for _ in range(10):
                if peer.rpc("mnsync", "status")["IsSynced"]:
                    break
                peer.rpc("mnsync", "next")
        for left in [mn, *others]:
            for right in [controller, mn, *others]:
                if left is not right:
                    left.rpc("addnode", f"127.0.0.1:{right.p2p}", "onetry")
        controller.rpc("sporkupdate", "SPORK_17_QUORUM_DKG_ENABLED", 0)
        controller.rpc("sporkupdate", "SPORK_21_QUORUM_ALL_CONNECTED", 4070908800)
        mn.stop()
        dkg = []
        for offset in range(args.pose_rounds * 24):
            mocktime = 1700001000 + offset * 10
            for peer in [controller, *others]:
                peer.rpc("setmocktime", mocktime)
            mine()
            height = controller.rpc("getblockcount")
            for peer in others:
                wait(lambda: peer.rpc("getblockcount") == height, "offline quorum sync")
            offline_blocks.append({"height": height, "block": controller.rpc("getbestblockhash"),
                                   "list": controller.rpc("protx", "list", "registered", True),
                                   "info": controller.rpc("protx", "info", pro), "status": None, "logs": []})
            phase = height % 24
            if phase in [0, 2, 4, 6, 8, 10, 12]:
                time.sleep(3)
                dkg.append({"height": height, "nodes": [peer.rpc("quorum", "dkgstatus") for peer in [controller, *others]]})
            if controller.rpc("protx", "info", pro)["state"]["PoSeBanHeight"] >= 0:
                break
        (args.work / "dkg.json").write_text(json.dumps(dkg, indent=2) + "\n")
        mn.start()
        mn.rpc("addnode", f"127.0.0.1:{controller.p2p}", "onetry")
        capture("offline_restart")
        if rows[-1]["info"]["state"]["PoSeBanHeight"] < 0:
            missing.append({"step": "pose_ban", "reason": f"Core parity: active/masternode.cpp:140-142; evo/deterministicmns.cpp:281-308. The remaining three fixture MNs ran {args.pose_rounds} DKG cycles while the target was stopped; no PoSe ban was observed. Raw quorum dkgstatus observations are in dkg.json."})
    finally:
        for node in reversed(nodes):
            node.stop()
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps({"core_commit": "938fba502824b9d850b7c710601456dc3d71603e",
            "version": version, "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
            "network_sha256": hashlib.sha256((args.network / "network.json").read_bytes()).hexdigest(),
            "rows": rows, "offline_blocks": offline_blocks, "missing": missing}, indent=2) + "\n")


if __name__ == "__main__":
    main()
