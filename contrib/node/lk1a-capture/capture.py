#!/usr/bin/env python3
"""Capture release-v24.0.0 ChainLock intake observations as JSONL vectors.

A v24.0.0 masternode network (controller plus four masternodes, built by the
v24.0.0 functional test framework) is mined across V19 (``v19@300``; V20 at the
framework's default height 100). Gossiped CLSIGs exist on both sides of V19;
coinbases carry ChainLocks only from V19 on, because the v24.0.0 miner blanks a
best ChainLock below the V19 height (node/miner.cpp:120-123). A listener peer on
the controller collects every CLSIG the network signs, as wire bytes.

The chain and the CLSIGs are then replayed into two fresh, observed nodes:

* N-cb has no peers (``-connect=0``; the peer count is checked after every
  block). After mnsync is forced past BLOCKCHAIN and SPORK_19 is set locally, it
  is fed every block with ``submitblock``, so each best-ChainLock change comes
  from the coinbase route.
* N-p2p has exactly one peer, the scripted peer. Each case is announced with
  ``inv`` and its CLSIG is sent only if N-p2p replies with ``getdata``
  (v24.0.0 penalises unsolicited CLSIGs); a ping/pong closes each exchange.

After each case the node is observed: mnsync, tip and IBD, SPORK_19, then
``syncwithvalidationinterfacequeue`` and the post-queue ``getbestchainlock``,
plus its chainlock, CLSIG-announcement and misbehaviour debug lines. Each case's
outcome is derived from those observations and the capture fails if it is not
the expected one.

Usage::

    eval "$(DASHVERSION=24.0.0 python3 contrib/setup-dashd.py)"
    python3 contrib/node/lk1a-capture/capture.py \\
        --dash-src <dash git checkout with tag v24.0.0> \\
        --capture-dir <new empty scratch dir> \\
        dashd-locks/tests/test-vectors/v24.0.0
"""
from __future__ import annotations

import argparse
import configparser
import hashlib
import io
import json
import os
import pathlib
import subprocess
import sys
import tarfile
import threading

VERSION = "24.0.0"
TAG = "v24.0.0"
V19_HEIGHT = 300
MSG_CLSIG = 29
SIGNING_OFFSET = 8  # SIGN_HEIGHT_OFFSET, llmq/quorumsman.h:214
STATUS_NAMES = {0: "no_quorum", 1: "invalid"}
LOG_KEYS = ("[chainlocks]", "Misbehaving", "got inv: clsig")


def sha256d(data: bytes) -> bytes:
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("output", type=pathlib.Path)
    parser.add_argument("--dash-src", type=pathlib.Path, required=True,
                        help="Dash Core git checkout containing the v24.0.0 tag")
    parser.add_argument("--capture-dir", type=pathlib.Path, required=True,
                        help="scratch directory for the framework and datadirs (must not exist)")
    return parser.parse_args()


def export_framework(dash_src: pathlib.Path, capture_dir: pathlib.Path) -> tuple[pathlib.Path, str]:
    commit = subprocess.run(["git", "-C", str(dash_src), "rev-parse", f"{TAG}^{{commit}}"],
                            check=True, capture_output=True, text=True).stdout.strip()
    archive = subprocess.run(["git", "-C", str(dash_src), "archive", commit, "test/functional"],
                             check=True, capture_output=True).stdout
    root = capture_dir / "core"
    root.mkdir(mode=0o700)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(root, filter="data")
    return root / "test" / "functional", commit


def write_framework_config(capture_dir: pathlib.Path, dashd: pathlib.Path) -> pathlib.Path:
    config = configparser.ConfigParser()
    config["environment"] = {"PACKAGE_NAME": "Dash Core", "PACKAGE_BUGREPORT": "",
                             "SRCDIR": str(capture_dir), "BUILDDIR": str(dashd.parent.parent),
                             "EXEEXT": "", "RPCAUTH": ""}
    config["components"] = {"ENABLE_WALLET": "true", "USE_SQLITE": "true", "USE_BDB": "true",
                            "ENABLE_CLI": "true", "ENABLE_BITCOIND": "true"}
    path = capture_dir / "config.ini"
    with open(path, "w") as file:
        config.write(file)
    return path


def main() -> int:
    args = parse_args()
    os.umask(0o077)
    dashd = pathlib.Path(os.environ.get("DASHD_PATH", ""))
    if not dashd.is_file():
        raise SystemExit("DASHD_PATH is unset; eval the output of DASHVERSION=24.0.0 contrib/setup-dashd.py")
    describe = subprocess.run([str(dashd), "--version"], check=True, capture_output=True,
                              text=True).stdout.splitlines()[0]
    if describe != f"Dash Core version {TAG}":
        raise SystemExit(f"expected a release {TAG} dashd, got {describe!r}")
    cli = dashd.with_name("dash-cli")
    args.capture_dir.mkdir(mode=0o700, parents=False)
    functional, commit = export_framework(args.dash_src, args.capture_dir)
    configfile = write_framework_config(args.capture_dir, dashd)
    sys.path.insert(0, str(functional))
    os.environ["DASHD"] = str(dashd)
    os.environ["DASHCLI"] = str(cli)
    provenance = {
        "describe": describe,
        "dashd_sha256": hashlib.sha256(dashd.read_bytes()).hexdigest(),
        "framework_commit": commit,
    }
    sys.argv = [sys.argv[0], f"--configfile={configfile}", f"--tmpdir={args.capture_dir / 'run'}",
                f"--cachedir={args.capture_dir / 'cache'}", "--nocleanup"]
    capture = build_capture_class()()
    capture.lk1a_output = args.output
    capture.lk1a_provenance = provenance
    capture.main()
    return 0


def build_capture_class():
    from test_framework.authproxy import JSONRPCException
    from test_framework.messages import CBlock, CCbTx, CInv, from_hex, msg_clsig, msg_getdata, msg_inv, msg_ping
    from test_framework.p2p import P2PInterface
    from test_framework.test_framework import DashTestFramework
    from test_framework.util import append_config, force_finish_mnsync, initialize_datadir

    class Recorder(P2PInterface):
        """Records every message on the connection while ``frames`` is set; requests nothing."""

        def __init__(self):
            super().__init__()
            self.frames: list[dict] | None = None
            # Guards frames between the network thread and the test thread. Not p2p_lock: the
            # framework already holds that while it dispatches and replies to a message.
            self.frames_lock = threading.Lock()

        def _log(self, direction: str, message) -> None:
            if self.frames is not None:
                self.frames.append({"dir": direction, "command": message.msgtype.decode("ascii"),
                                    "payload": message.serialize().hex()})

        def on_message(self, message):
            with self.frames_lock:
                self._log("recv", message)
            super().on_message(message)

        def send_message(self, message, is_decoy=False):
            with self.frames_lock:
                self._log("send", message)
            return super().send_message(message, is_decoy)

        def on_inv(self, message):
            pass

    class Listener(P2PInterface):
        """Requests every announced CLSIG from the controller and keeps its wire bytes by height."""

        def __init__(self):
            super().__init__()
            self.clsigs: dict[int, bytes] = {}

        def on_inv(self, message):
            want = [inv for inv in message.inv if inv.type == MSG_CLSIG]
            if want:
                self.send_message(msg_getdata(want))

        def on_clsig(self, message):
            self.clsigs[message.height] = message.serialize()

    class Capture(DashTestFramework):
        def add_options(self, parser):
            self.add_wallet_options(parser)

        def set_test_params(self):
            self.common_args = ["-deprecatedrpc=legacy_mn", f"-testactivationheight=v19@{V19_HEIGHT}",
                                "-debug=chainlocks", "-debug=net"]
            self.set_dash_test_params(5, 4, [list(self.common_args) for _ in range(5)])

        # --- controller network -------------------------------------------------------------

        def next_clsig(self, listener) -> bytes:
            """Mine one block, wait for the network to lock it, return its CLSIG wire bytes."""
            self.bump_mocktime(1)
            tip = self.generate(self.nodes[0], 1)[0]
            self.wait_for_chainlocked_block_all_nodes(tip, timeout=60)
            height = self.nodes[0].getblockcount()
            listener.wait_until(lambda: height in listener.clsigs, timeout=60)
            raw = listener.clsigs[height]
            assert raw[4:36] == bytes.fromhex(tip)[::-1]
            return raw

        def run_network(self) -> dict[str, bytes]:
            for i in range(2, len(self.nodes)):
                self.connect_nodes(i, 1)
            listener = self.nodes[0].add_p2p_connection(Listener())
            self.nodes[0].sporkupdate("SPORK_17_QUORUM_DKG_ENABLED", 0)
            self.wait_for_sporks_same()
            self.mine_quorum()
            self.wait_for_chainlocked_block_all_nodes(self.nodes[0].getbestblockhash(), timeout=60)
            cls = {"stale": self.next_clsig(listener), "legacy": self.next_clsig(listener)}
            assert self.nodes[0].getblockcount() < V19_HEIGHT - 1
            self.activate_by_name("v19", expected_activation_height=V19_HEIGHT)
            self.mine_quorum()
            self.mine_quorum()
            self.wait_for_chainlocked_block_all_nodes(self.nodes[0].getbestblockhash(), timeout=60)
            cls["basic"] = self.next_clsig(listener)
            basic_height = int.from_bytes(cls["basic"][:4], "little", signed=True)
            self.bump_mocktime(1)
            self.generate(self.nodes[0], SIGNING_OFFSET)
            cls["no_quorum"] = self.next_clsig(listener)
            assert int.from_bytes(cls["no_quorum"][:4], "little", signed=True) - SIGNING_OFFSET > basic_height
            return cls

        # --- observation helpers ------------------------------------------------------------

        def block_record(self, node, height: int) -> dict:
            block_hash = node.getblockhash(height)
            raw = node.getblock(block_hash, 0)
            return {"height": height, "hash": bytes.fromhex(block_hash)[::-1].hex(), "bytes": raw}

        def best_chainlock(self, node):
            try:
                return node.getbestchainlock()
            except JSONRPCException as error:
                if error.error.get("code") != -32603:  # "Unable to find any ChainLock"
                    raise
                return {"error": str(error)}

        def observe(self, node) -> dict:
            state = {"mnsync": node.mnsync("status")}
            info = node.getblockchaininfo()
            deployments = node.getdeploymentinfo()["deployments"]
            state.update({
                "tip_height": info["blocks"],
                "tip_hash": info["bestblockhash"],
                "ibd": info["initialblockdownload"],
                "spork_19": {"value": node.spork("show")["SPORK_19_CHAINLOCKS_ENABLED"],
                             "active": node.spork("active")["SPORK_19_CHAINLOCKS_ENABLED"]},
                "v19_active": deployments["v19"]["active"],
                "v20_active": deployments["v20"]["active"],
                "peer_count": node.getconnectioncount(),
            })
            state["syncwithvalidationinterfacequeue"] = node.syncwithvalidationinterfacequeue()
            state["getbestchainlock"] = self.best_chainlock(node)
            return state

        @staticmethod
        def scheme(state: dict) -> str:
            return "basic" if state["v19_active"] else "legacy"

        @staticmethod
        def derived(height: int) -> dict:
            return {"signing_start_height": height - SIGNING_OFFSET, "sign_height_offset": SIGNING_OFFSET}

        def debug_since(self, node, offset: int) -> list[str]:
            with open(node.debug_log_path, "rb") as log:
                log.seek(offset)
                text = log.read().decode("utf-8", errors="replace")
            return [line for line in text.splitlines() if any(key in line for key in LOG_KEYS)]

        def clsig_record(self, raw: bytes, scheme: str) -> dict:
            return {"bytes": raw.hex(), "height": int.from_bytes(raw[:4], "little", signed=True),
                    "block_hash": raw[4:36].hex(), "signature": raw[36:].hex(),
                    "hash": sha256d(raw).hex(), "scheme": scheme}

        def feed(self, node, blocks: list[dict]) -> list:
            results = [node.submitblock(block["bytes"]) for block in blocks]
            # Coinbase ChainLocks are processed on the validation queue; finish them before a case.
            node.syncwithvalidationinterfacequeue()
            return results

        def start_observed(self, extra: list[str]):
            idx = len(self.nodes)
            initialize_datadir(self.options.tmpdir, idx, self.chain, self.disable_autoconnect)
            self.add_nodes(1, extra_args=[self.common_args + ["-connect=0"] + extra])
            # SPORK_19 is set locally, with the regtest spork key the framework gave the controller.
            conf = pathlib.Path(self.nodes[0].datadir, "dash.conf").read_text().splitlines()
            append_config(self.nodes[idx].datadir, [line for line in conf if line.startswith("sporkkey=")])
            self.start_node(idx)
            node = self.nodes[idx]
            node.setmocktime(self.mocktime)
            return node

        def enable_chainlocks(self, node):
            force_finish_mnsync(node)
            assert node.mnsync("status")["IsBlockchainSynced"]
            node.sporkupdate("SPORK_19_CHAINLOCKS_ENABLED", 0)
            self.wait_until(lambda: node.spork("active")["SPORK_19_CHAINLOCKS_ENABLED"])

        @staticmethod
        def same_lock(rpc_value: dict, raw: bytes) -> bool:
            return (rpc_value.get("height") == int.from_bytes(raw[:4], "little", signed=True)
                    and rpc_value.get("blockhash") == raw[4:36][::-1].hex()
                    and rpc_value.get("signature") == raw[36:].hex())

        # --- N-cb ---------------------------------------------------------------------------

        def capture_cb(self, blocks: list[dict]) -> dict:
            node = self.start_observed([])
            self.enable_chainlocks(node)
            changes, chosen = [], None
            best = self.best_chainlock(node)
            for block in blocks:
                before_state = best
                offset = node.debug_log_size(mode="rb")
                result = node.submitblock(block["bytes"])
                assert result is None, result
                state = self.observe(node)
                assert state["peer_count"] == 0
                best = state["getbestchainlock"]
                if best == before_state:
                    continue
                changes.append({"carrier_height": block["height"], "chainlock_height": best["height"]})
                if chosen is not None:
                    continue
                assert best["height"] >= V19_HEIGHT, best  # coinbases carry no pre-V19 ChainLock
                carrier = from_hex(CBlock(), block["bytes"])
                cbtx = CCbTx()
                cbtx.deserialize(io.BytesIO(carrier.vtx[0].vExtraPayload))
                height = block["height"] - cbtx.bestCLHeightDiff - 1
                raw = (height.to_bytes(4, "little", signed=True)
                       + bytes.fromhex(blocks[height - 1]["hash"]) + cbtx.bestCLSignature)
                assert self.same_lock(best, raw), (best, raw.hex())
                chosen = {
                    "case": "coinbase-route-best-change", "node": "N-cb", "outcome": "valid",
                    "block": blocks[height - 1], "clsig": self.clsig_record(raw, self.scheme(state)),
                    "input": {"route": "submitblock", "carrier_block": block,
                              "submitblock_result": result, "exchange": []},
                    "state": dict(state, getbestchainlock_before=before_state),
                    "derived": self.derived(height),
                    "handler_debug": self.debug_since(node, offset),
                }
            assert chosen is not None, "no coinbase-route best ChainLock change on N-cb"
            chosen["input"]["best_chainlock_changes"] = changes
            return chosen

        # --- N-p2p --------------------------------------------------------------------------

        def ping(self, peer):
            nonce = peer.ping_counter
            peer.ping_counter += 1
            peer.send_message(msg_ping(nonce=nonce))
            peer.wait_until(lambda: peer.last_message.get("pong") is not None
                            and peer.last_message["pong"].nonce == nonce, timeout=60)

        def deliver(self, node, peer, case: str, raw: bytes, blocks: list[dict], expected: str) -> dict:
            before = self.best_chainlock(node)
            offset = node.debug_log_size(mode="rb")
            hash_int = int.from_bytes(sha256d(raw), "little")
            frames: list[dict] = []
            with peer.frames_lock:
                peer.frames = frames
            peer.send_message(msg_inv([CInv(MSG_CLSIG, hash_int)]))
            self.ping(peer)
            with peer.frames_lock:
                requested = any(frame["dir"] == "recv" and frame["command"] == "getdata"
                                and CInv(MSG_CLSIG, hash_int).serialize().hex() in frame["payload"]
                                for frame in frames)
            if requested:
                message = msg_clsig()
                message.deserialize(io.BytesIO(raw))
                peer.send_message(message)
                self.ping(peer)
            with peer.frames_lock:
                peer.frames = None
            state = self.observe(node)
            assert state["peer_count"] == 1
            debug = self.debug_since(node, offset)
            announced = [line.split(f"clsig {sha256d(raw)[::-1].hex()}")[1].split()[0] for line in debug
                         if f"got inv: clsig {sha256d(raw)[::-1].hex()}" in line]
            assert announced == ["have" if not requested else "new"], (case, requested, announced)
            statuses = [int(line.split("status=")[1].split()[0]) for line in debug
                        if "invalid CLSIG" in line and "status=" in line]
            if not requested:
                outcome = "duplicate"
            elif self.same_lock(state["getbestchainlock"], raw):
                outcome = "valid"
            elif statuses:
                outcome = STATUS_NAMES[statuses[-1]]
            elif state["getbestchainlock"] == before:
                outcome = "stale"
            else:
                outcome = "unexpected"
            assert outcome == expected, (case, outcome, expected, debug)
            assert outcome == "valid" or state["getbestchainlock"] == before, (case, before, state)
            height = int.from_bytes(raw[:4], "little", signed=True)
            record = {
                "case": case, "node": "N-p2p", "outcome": outcome,
                "block": blocks[height - 1], "clsig": self.clsig_record(raw, self.scheme(state)),
                "input": {"route": "p2p", "requested": requested, "exchange": frames},
                "state": dict(state, getbestchainlock_before=before),
                "derived": self.derived(height),
                "handler_debug": debug,
            }
            return record

        def capture_p2p(self, blocks: list[dict], cls: dict[str, bytes]) -> list[dict]:
            node = self.start_observed(["-listen=1"])
            legacy_height = int.from_bytes(cls["legacy"][:4], "little", signed=True)
            basic_height = int.from_bytes(cls["basic"][:4], "little", signed=True)
            assert all(result is None for result in self.feed(node, blocks[:legacy_height]))
            assert "error" in self.best_chainlock(node)
            self.enable_chainlocks(node)
            peer = node.add_p2p_connection(Recorder())
            self.ping(peer)
            out = [
                self.deliver(node, peer, "pre-v19-legacy-valid", cls["legacy"], blocks, "valid"),
                self.deliver(node, peer, "p2p-duplicate-clsig", cls["legacy"], blocks, "duplicate"),
                self.deliver(node, peer, "p2p-stale-clsig", cls["stale"], blocks, "stale"),
            ]
            assert all(result is None for result in self.feed(node, blocks[legacy_height:basic_height]))
            self.ping(peer)
            flipped = bytearray(cls["basic"])
            flipped[36] ^= 0x20  # the compressed-point sign flag: still a curve point, wrong signature
            out += [
                self.deliver(node, peer, "p2p-one-bit-flipped-signature", bytes(flipped), blocks, "invalid"),
                self.deliver(node, peer, "post-v19-basic-valid", cls["basic"], blocks, "valid"),
                self.deliver(node, peer, "p2p-no-quorum-past-tip", cls["no_quorum"], blocks, "no_quorum"),
            ]
            assert [r["clsig"]["scheme"] for r in out] == ["legacy"] * 3 + ["basic"] * 3
            out[3]["clsig"]["bit_flip"] = {"byte": 36, "mask": 0x20,
                                           "original_signature": cls["basic"][36:].hex()}
            return out

        def run_test(self):
            cls = self.run_network()
            tip = self.nodes[0].getblockcount()
            blocks = [self.block_record(self.nodes[0], height) for height in range(1, tip + 1)]
            records = self.capture_p2p(blocks, cls)
            records.append(self.capture_cb(blocks))
            output = self.lk1a_output
            output.mkdir(parents=True, exist_ok=True)
            with open(output / "chainlocks.jsonl", "w") as file:
                for item in records:
                    file.write(json.dumps(item, sort_keys=True) + "\n")
            prov = self.lk1a_provenance
            (output / "PROVENANCE.md").write_text(provenance_text(prov, tip, records))

    return Capture


def provenance_text(prov: dict, tip: int, records: list[dict]) -> str:
    cases = "\n".join(f"| `{r['case']}` | {r['node']} | {r['outcome']} | {r['clsig']['height']} | "
                      f"{r['clsig']['scheme']} | {r['state']['tip_height']} |" for r in records)
    return f"""# LK1a C2 provenance

Captured by `contrib/node/lk1a-capture/capture.py` from our own regtest network.

- dashd: release `{prov['describe']}`, from `contrib/setup-dashd.py` with `DASHVERSION={VERSION}`
- dashd SHA-256: `{prov['dashd_sha256']}`
- Network driver: Dash Core functional test framework at tag `{TAG}` (`{prov['framework_commit']}`),
  controller plus four masternodes, `-testactivationheight=v19@{V19_HEIGHT}`, V20 at the framework's
  default height 100
- Controller chain height at capture end: {tip}

Gossiped CLSIGs exist on both sides of V19, but v24.0.0 coinbases carry ChainLocks only from V19 on
(the miner blanks a best ChainLock below the V19 height, node/miner.cpp:120-123).

N-cb has no peers and is fed every block by `submitblock` after mnsync is forced past BLOCKCHAIN
and SPORK_19 is set locally; its record is its first best-ChainLock change (all changes are listed
in `input.best_chainlock_changes`). N-p2p's only peer is the scripted peer; each case is an `inv`,
then the CLSIG if N-p2p asked for it with `getdata`, each followed by a ping/pong, and
`input.exchange` holds that case's messages in both directions (received payloads re-serialized by
the framework). N-p2p's `got inv: clsig ... have|new` debug line must agree with whether it asked.
N-p2p is fed blocks 1..hL before SPORK_19 is set, and blocks hL+1..hB between the legacy and the
basic cases; the coinbase route moves its best ChainLock forward during that feed, before the
bit-flip case (the value is in that case's `getbestchainlock_before`).

`state` is observed after each case: mnsync, tip and IBD, SPORK_19, then
`syncwithvalidationinterfacequeue` and the post-queue `getbestchainlock`; `scheme` follows the
observed `v19_active`. `block` is the block the CLSIG names. `derived` holds values computed from
the CLSIG height, not observed: the signing quorum start height h - 8 (SIGN_HEIGHT_OFFSET,
llmq/quorumsman.h:214).

| case | node | outcome | CLSIG height | scheme | node tip |
|---|---|---|---|---|---|
{cases}
"""


if __name__ == "__main__":
    raise SystemExit(main())
