#!/usr/bin/env python3
"""IM1a vector capture: drive a real Dash Core v24.0.0-rc.3 regtest dashd with Core's own
functional-test framework, stop it cleanly and keep the net-state files it wrote.

Not part of Core. Run (see the PROVENANCE.md next to the vectors):
  python3 im1a_capture.py --configfile=<core>/test/config.ini --tmpdir=<dir> --nocleanup --outdir=<out>

Output layout (--outdir):
  empty/      the files of a node that was started and stopped immediately
  populated/  the files after the state built below, plus expected.json, taken from RPCs
              (ground truth that does not come from this repo's code)
"""
import json
import os
import shutil

from test_framework.messages import CAddress
from test_framework.p2p import P2PInterface
from test_framework.socks5 import Socks5Configuration, Socks5Server
from test_framework.test_framework import BitcoinTestFramework
from test_framework.util import p2p_port
from test_framework.wallet import MiniWallet

ONION_ADDR = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:8333"
FILES = ["peers.dat", "anchors.dat", "banlist.json", "mempool.dat", "fee_estimates.dat", "settings.json"]


class Im1aCapture(BitcoinTestFramework):
    def add_options(self, parser):
        parser.add_argument("--outdir", dest="outdir", required=True)
        self.add_wallet_options(parser, legacy=False)

    def set_test_params(self):
        self.num_nodes = 1
        self.setup_clean_chain = True
        self.disable_autoconnect = False

    def regtest_dir(self):
        return os.path.join(self.nodes[0].datadir, "regtest")

    def copy_files(self, name):
        dest = os.path.join(self.options.outdir, name)
        os.makedirs(dest, exist_ok=True)
        for f in FILES:
            src = os.path.join(self.regtest_dir(), f)
            if os.path.exists(src):
                shutil.copy(src, os.path.join(dest, f))
        return dest

    def run_test(self):
        node = self.nodes[0]

        self.log.info("empty: start + stop")
        self.stop_node(0)
        self.copy_files("empty")
        self.start_node(0)
        node = self.nodes[0]

        self.log.info("fee estimation history: txs at several feerates over many blocks")
        wallet = MiniWallet(node)
        self.generate(wallet, 120)
        for i in range(60):
            for rate_mult in (1, 2, 5, 10, 25):
                wallet.send_self_transfer(from_node=node, fee_rate=wallet_rate(rate_mult))
            self.generate(wallet, 1)

        self.log.info("peers.dat: addrman entries over several networks, new + tried")
        added = []

        def add(addr, port, tried=False):
            r = node.addpeeraddress(address=addr, port=port, tried=tried)
            added.append({"address": addr, "port": port, "tried": tried, "success": r["success"]})

        for i in range(1, 40):
            add(f"{20 + i}.{i}.{i}.{i}", 9999, tried=(i % 4 == 0))
        add("21.1.1.1", 19999)  # same IP, second port (V4_MULTIPORT)
        add("2001:db8:85a3::8a2e:370:7334", 9999)  # documentation prefix: invalid, not stored
        add("2a01:4f8:1c0c:8212::1", 9999, tried=True)
        add("2a01:4f8:1c0c:8212::2", 9999)
        add("pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion", 9999)
        add("ukeu3k5oycgaauneqgtnvselmt4yemvoilkln7jpvamvfx7dnkdq.b32.i2p", 0)
        add("ukeu3k5oycgaauneqgtnvselmt4yemvoilkln7jpvamvfx7dnkdq.b32.i2p", 0, tried=True)

        self.log.info("banlist.json")
        node.setban("192.168.0.6", "add")
        node.setban("10.0.0.0/8", "add", 1893456000, True)
        node.setban("2a01:4f8:1c0c:8212::/64", "add")
        node.setban("1.2.3.4/255.255.0.0", "add")
        node.setban("pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion", "add")

        self.log.info("settings.json: a wallet loaded on startup")
        node.createwallet(wallet_name="im1a", load_on_startup=True)
        node.unloadwallet("im1a", load_on_startup=True)  # leaves a non-trivial 'wallet' key list
        node.createwallet(wallet_name="im1a2", load_on_startup=True)

        self.log.info("mempool.dat: txs, a delta on a pooled tx, a delta on an absent tx, unbroadcast set")
        txs = [wallet.send_self_transfer(from_node=node, fee_rate=wallet_rate(3 + i)) for i in range(4)]
        node.prioritisetransaction(txid=txs[0]["txid"], fee_delta=12345)
        node.prioritisetransaction(txid="ab" * 32, fee_delta=-777)
        node.prioritisetransaction(txid="cd" * 32, fee_delta=2**40)
        # a tx with a special-tx type would need the DIP2 payload machinery; the txs above are plain.

        self.log.info("anchors.dat: one block-relay-only P2P peer and one onion peer through SOCKS")
        self.stop_node(0)
        onion_conf = Socks5Configuration()
        onion_conf.auth = True
        onion_conf.unauth = True
        onion_conf.addr = ("127.0.0.1", p2p_port(self.num_nodes))
        onion_conf.keep_alive = True
        onion_proxy = Socks5Server(onion_conf)
        onion_proxy.start()
        # restart keeps peers.dat, banlist.json, mempool.dat and fee_estimates.dat (loaded again)
        self.start_node(0, extra_args=[f"-onion={onion_conf.addr[0]}:{onion_conf.addr[1]}"])
        node = self.nodes[0]
        node.add_outbound_p2p_connection(P2PInterface(), p2p_idx=0, connection_type="block-relay-only")
        node.addconnection(ONION_ADDR, "block-relay-only", v2transport=False)

        expected = {
            "addpeeraddress": added,
            "rawaddrman": node.getrawaddrman(),
            "nodeaddresses": node.getnodeaddresses(0),
            "listbanned": node.listbanned(),
            "rawmempool": node.getrawmempool(True),
            "mempool_entries": {t: node.getmempoolentry(t) for t in node.getrawmempool()},
            "estimaterawfee": {str(t): node.estimaterawfee(t) for t in (2, 6, 25, 100, 500)},
            "blockcount": node.getblockcount(),
            "peerinfo": [
                {"addr": p["addr"], "connection_type": p["connection_type"]} for p in node.getpeerinfo()
            ],
        }
        self.stop_node(0)
        onion_proxy.stop()
        dest = self.copy_files("populated")
        with open(os.path.join(dest, "expected.json"), "w") as fh:
            json.dump(expected, fh, indent=1, sort_keys=True, default=str)


def wallet_rate(mult):
    from decimal import Decimal

    return Decimal("0.00001") * mult


if __name__ == "__main__":
    Im1aCapture().main()
