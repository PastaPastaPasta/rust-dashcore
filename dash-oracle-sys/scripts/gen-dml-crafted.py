#!/usr/bin/env python3
"""Capture Core's verdicts on crafted regtest blocks for oracle_dml_block (node-program O2a-EV),
one block per masternode-list operator of EV2 §6.5 (tests/data/dml-crafted-<pin>.json).

Runs a release dashd (`--dashd`, the pin's official binary) on two fresh regtest chains with
the default parameters (DIP0003 and V20 at 432, its enforcement at 500, V19 from height 1),
the second with V24 always active (`-vbparams=v24:-1:999999999999`), both mined past 510. On
the first it registers a few masternodes the ordinary way (mined); the second only holds the
duplicate-operator-key case, which needs none. Then for every case it:
- has Core build the provider transactions with `submit=false` (signed, not broadcast), each
  funded from an address of its own so no two spend one coin;
- mines a block on the tip and takes it back with invalidateblock (so its coinbase is the
  one Core made for that height), adds the case's transactions after the coinbase and
  submits the result as a getblocktemplate proposal. Core answers with TestBlockValidity()'s
  reason; ConnectBlock runs ProcessSpecialTxsInBlock before any script check, so the
  masternode-list verdict comes first. A block whose list update Core accepts fails later,
  at the CbTx's merkleRootMNList (`bad-cbtx-mnmerkleroot`), as the coinbase was made for the
  block without the case's transactions; a commitment can also fail in the quorum block
  processor (`bad-qc-block`), which runs between the check loop and BuildNewListFromBlock
  and which oracle_dml_block leaves out;
- records the oracle context of the tip: its height (the headers are the chain's, recorded
  once in `chains`), the tip's list as evodb stores it (read
  with `protx listdiff 1 <tip>`, encoded by dml-listdiff-check.py), the coins the block's
  ProRegTxs name as external collateral (at the tip, from getrawtransaction), the V24 state
  for the next block and the mock time.
Nothing here is an expected value of the script's own: each `core_reason` is the release's
answer. `stage` is where in ProcessSpecialTxsInBlock Core gives that reason (the check loop or
BuildNewListFromBlock), from reading the pin's specialtxman.cpp; the test checks the oracle
reports it.

Usage: gen-dml-crafted.py --dashd <release>/bin/dashd --pin v24.0 --datadir DIR --port BASE \\
           > tests/data/dml-crafted-v24.0.json
"""

import argparse
import base64
import importlib.util
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


regtest = load("gen_contextual_regtest", "gen-contextual-regtest.py")
listdiff = load("dml_listdiff_check", "dml-listdiff-check.py")

# Regtest's parameters (oracle_chainparams_json at the pin), for the list encoding.
NET = {
    "chain": "regtest",
    "dip3_height": 432,
    "v19_height": 1,
    "v20_height": 432,
    "min_confirmations": 1,
    "pubkey_prefix": 140,
    "script_prefix": 19,
    "llmqs": {},
    "platform_llmq": 106,
    "rotated_llmqs": {},
}

# What a transaction's funding address holds: an evonode or regular collateral plus fees, or
# fees alone.
EVO, REG, FEE = 4001, 1001, 1

CHECK = "check"  # the CheckSpecialTxInner loop
BUILD = "build"  # BuildNewListFromBlock
LATER = None  # Core rejects outside the check loop and the rebuild (quorum block processor, merkle roots)


class Case:
    def __init__(self, chain, keys):
        self.chain = chain
        self.node = chain.node
        self.keys = keys

    def call(self, method, *params):
        return self.node.call(method, *params, wallet="w")


class Regtest:
    """One regtest chain: a release dashd, a wallet, masternodes registered the ordinary way."""

    def __init__(self, dashd, datadir, port, v24):
        node = regtest.Node.__new__(regtest.Node)
        self.node = node
        extra = ["-txindex=1"] + (["-vbparams=v24:-1:999999999999"] if v24 else [])
        start_node(node, dashd, datadir, port, extra)
        self.chain = regtest.Chain(node)
        self.v24 = v24
        self.funded = []
        # Past DIP0003 enforcement (500), where owner and voting keys may differ.
        self.chain.mine(510)

    def call(self, method, *params):
        return self.node.call(method, *params, wallet="w")

    def fund(self, amounts):
        """Fresh addresses with one coin of each amount (mined), one per transaction to build.
        The coins stay locked, so no other spend of the wallet takes them, until the address
        is drawn from the returned iterator."""
        addresses = [self.call("getnewaddress") for _ in amounts]
        self.call("sendmany", "", dict(zip(addresses, amounts)))
        self.chain.mine(1)
        coins = {a: [{"txid": u["txid"], "vout": u["vout"]} for u in self.call("listunspent", 1, 9999999, [a])]
                 for a in addresses}
        self.call("lockunspent", False, [c for cs in coins.values() for c in cs])

        def draw():
            for a in addresses:
                self.call("lockunspent", True, coins[a])
                yield a

        return draw()

    def bls(self):
        return self.call("bls", "generate")

    def keys(self):
        """Owner, voting and payout addresses and an operator key pair (basic scheme)."""
        return {
            "owner": self.call("getnewaddress"),
            "voting": self.call("getnewaddress"),
            "payout": self.call("getnewaddress"),
            "operator": self.bls(),
        }

    def register(self, service, fund, submit=True, keys=None, collateral=None):
        """A regular ProRegTx funding its own collateral (register_fund)."""
        k = keys or self.keys()
        collateral = collateral or self.call("getnewaddress")
        out = self.call("protx", "register_fund", collateral, [service] if service else [], k["owner"],
                        k["operator"]["public"], k["voting"], 0, k["payout"], fund, submit)
        return k, out

    def register_evo(self, service, platform_node_id, fund, submit=True, keys=None):
        k = keys or self.keys()
        collateral = self.call("getnewaddress")
        host = service.rsplit(":", 1)[0]
        out = None
        for p2p, https in platform_addresses(host):
            try:
                out = self.call("protx", "register_fund_evo", collateral, [service], k["owner"],
                                k["operator"]["public"], k["voting"], 0, k["payout"], platform_node_id, p2p, https,
                                fund, submit)
                break
            except RuntimeError as e:
                if "platform" not in str(e):
                    raise
        assert out is not None, "register_fund_evo"
        return k, out

    def tip(self):
        return self.chain.tip()

    def context(self, block_txs):
        """The oracle context of the tip for a block holding block_txs (raw bytes)."""
        tip = self.tip()
        ld_chain = listdiff.Chain(self.node, NET)
        mns = ld_chain.list_json(tip)
        coins = []
        for raw in block_txs:
            tx = self.node.call("decoderawtransaction", raw.hex())
            if tx.get("type") == 1 and int(tx["proRegTx"]["collateralHash"], 16) != 0:
                try:
                    coins.append(listdiff.coin_of(ld_chain, tx["proRegTx"]["collateralHash"],
                                                  tx["proRegTx"]["collateralIndex"]))
                except RuntimeError:
                    pass  # not at the tip (created in the block itself)
        info = self.node.call("getdeploymentinfo")["deployments"]["v24"]
        status = info.get("bip9", {}).get("status_next", info.get("bip9", {}).get("status"))
        states = {"defined": 0, "started": 1, "locked_in": 2, "active": 3, "failed": 4}
        return {
            # The headers are the chain's (`chains`), 0 .. tip.
            "tip": tip,
            "list": listdiff.encode_list(mns, tip, ld_chain).hex(),
            "coins": coins,
            "deployments": [[1, states[status]]],
            "adjusted_time": self.chain.now,
            "bls_legacy": False,
        }


def platform_addresses(host):
    """The platform address arguments: bare ports below ProTx version 3 (ExtAddr), address
    arrays from it; the RPC accepts one form per version."""
    return [("26656", "443"), ([f"{host}:26656"], [f"{host}:443"])]


def start_node(node, dashd, datadir, port, extra):
    """regtest.Node.__init__ with extra arguments."""
    import shutil
    import subprocess
    import time

    node.datadir = Path(datadir)
    shutil.rmtree(node.datadir, ignore_errors=True)
    node.datadir.mkdir(parents=True)
    node.rpc_port = port + 1
    args = [dashd, "-regtest", f"-datadir={node.datadir}", f"-port={port}", f"-rpcport={node.rpc_port}",
            "-rpcuser=o2a", "-rpcpassword=o2a", "-listen=0", "-listenonion=0", "-server", "-fallbackfee=0.0001",
            "-printtoconsole=0", f"-mocktime={regtest.MOCK_START}"] + extra
    node.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    node.auth = base64.b64encode(b"o2a:o2a").decode()
    for _ in range(120):
        try:
            node.call("getblockcount")
            return
        except Exception:
            time.sleep(1)
    raise RuntimeError("dashd did not start")


def raw(hex_tx):
    return bytes.fromhex(hex_tx)


def signed(r, hex_tx):
    out = r.call("signrawtransactionwithwallet", hex_tx)
    assert out["complete"], out
    return out["hex"]


def payload_start(tx):
    """The offset of a special transaction's extra payload."""
    j = regtest.tx_body_end(tx, 0)
    _, j = regtest.read_compact(tx, j)
    return j


def set_payload(tx, payload):
    j = regtest.tx_body_end(tx, 0)
    return tx[:j] + regtest.compact_size(len(payload)) + payload


def craft(r, txs):
    """Core's next block on the tip, taken back, with txs added after its coinbase."""
    block = r.chain.next_block()
    header, mined = regtest.split_block(block)
    return regtest.join_block(header, mined + txs)


def record(r, name, operator, txs, stage, note):
    block = craft(r, txs)
    reason = r.chain.propose(block)
    case = {
        "name": name,
        "operator": operator,
        "v24": r.v24,
        "height": r.tip() + 1,
        "context": r.context(txs),
        "block": block.hex(),
        "core_reason": reason,
        "stage": stage,
        "note": note,
    }
    print(f"{name}: {reason}", file=sys.stderr, flush=True)
    return case


def setup(r):
    """Masternodes registered and mined: a regular one funding its collateral, one with an
    external collateral, an evonode."""
    evo_fund, reg_fund, fee_fund = r.fund([EVO]), r.fund([REG]), r.fund([FEE])
    mn1, _ = r.register("127.0.0.1:10001", next(reg_fund))
    collateral = r.call("getnewaddress")
    r.call("sendtoaddress", collateral, 1000)
    r.chain.mine(1)
    utxo = next(u for u in r.call("listunspent", 1, 9999999, [collateral]) if u["amount"] == 1000)
    mn2 = r.keys()
    prep = r.call("protx", "register_prepare", utxo["txid"], utxo["vout"], ["127.0.0.1:10002"], mn2["owner"],
                  mn2["operator"]["public"], mn2["voting"], 0, mn2["payout"], next(fee_fund))
    sig = r.call("signmessage", prep["collateralAddress"], prep["signMessage"])
    r.call("protx", "register_submit", prep["tx"], sig)
    evo, _ = r.register_evo("127.0.0.1:10003", "11" * 20, next(evo_fund))
    r.chain.mine(3)
    mns = {m["state"]["payoutAddress"]: m["proTxHash"] for m in r.call("protx", "list", "registered", True)}
    return {
        "mn1": dict(mn1, protx=mns[mn1["payout"]]),
        "mn2": dict(mn2, protx=mns[mn2["payout"]], collateral=(utxo["txid"], utxo["vout"]),
                    collateral_address=collateral),
        "evo": dict(evo, protx=mns[evo["payout"]]),
    }


def cases_common(r):
    out = []
    f = r.fund([REG, REG])

    # Duplicate operator key in one block (the same encoding: GetUniquePropertyHash).
    k = r.keys()
    k2 = dict(r.keys(), operator=k["operator"])
    _, a = r.register("127.0.0.1:11001", next(f), False, keys=k)
    _, b = r.register("127.0.0.1:11002", next(f), False, keys=k2)
    out.append(record(r, "dup_operator_key" + ("_v24" if r.v24 else ""), "duplicate operator key in-block",
                      [raw(a), raw(b)], BUILD, "the second ProRegTx's pubKeyOperator is the first's"))
    return out


def cases_pre_v24(r, mns):
    out = []
    evo_f = r.fund([EVO] * 4)
    f = r.fund([REG] * 4)
    fee = r.fund([FEE] * 7)
    ip = iter(range(12001, 12100))

    def service():
        return f"127.0.0.1:{next(ip)}"

    # Duplicate platformNodeID in one block.
    _, a = r.register_evo(service(), "22" * 20, next(evo_f), False)
    _, b = r.register_evo(service(), "22" * 20, next(evo_f), False)
    out.append(record(r, "dup_platform_node_id", "duplicate platformNodeID in-block", [raw(a), raw(b)], BUILD,
                      "two evonode ProRegTxs with one platformNodeID"))

    # keyIDOwner of one ProRegTx equal to the platformNodeID of another, both orders.
    for order in ("keyid_first", "platform_first"):
        owner = r.call("getnewaddress")
        key_id = r.call("getaddressinfo", owner)["scriptPubKey"][6:46]
        _, reg = r.register(service(), next(f), False, keys=dict(r.keys(), owner=owner))
        # The payload must hold the key id's bytes; whichever order the RPC parses.
        fund_evo = next(evo_f)
        for node_id in (key_id, bytes.fromhex(key_id)[::-1].hex()):
            _, evo = r.register_evo(service(), node_id, fund_evo, False)
            if raw(evo)[payload_start(raw(evo)):].find(bytes.fromhex(key_id)) >= 0:
                break
        else:
            raise RuntimeError("platformNodeID bytes")
        txs = [raw(reg), raw(evo)] if order == "keyid_first" else [raw(evo), raw(reg)]
        out.append(record(r, f"keyid_platform_{order}", "keyID and platformNodeID, " + order.replace("_", " "),
                          txs, BUILD, "one ProRegTx's keyIDOwner bytes are another's platformNodeID"))

    # Duplicate service in one block.
    s = service()
    _, a = r.register(s, next(f), False)
    _, b = r.register(s, next(f), False)
    out.append(record(r, "dup_service", "duplicate service in-block", [raw(a), raw(b)], BUILD,
                      "two ProRegTxs with one service"))

    # A collateral created in the same block: the funding transaction, then a ProRegTx naming
    # its output. register_prepare reads only confirmed collaterals, so the ProRegTx is
    # prepared for a confirmed coin, then its collateralOutpoint is pointed at the funding
    # output and its payload signed again by that output's key over the new sign string
    # (MakeSignString ends with the payload's SerializeHash, which leaves vchSig out).
    confirmed_collateral = r.call("getnewaddress")
    r.call("sendtoaddress", confirmed_collateral, 1000)
    r.chain.mine(1)
    utxo = next(u for u in r.call("listunspent", 1, 9999999, [confirmed_collateral]) if u["amount"] == 1000)
    collateral = r.call("getnewaddress")
    funding = r.call("createrawtransaction", [], {collateral: 1000})
    funding = signed(r, r.call("fundrawtransaction", funding, {"changePosition": 1})["hex"])
    funding_txid = r.node.call("decoderawtransaction", funding)["txid"]
    k = r.keys()
    prep = r.call("protx", "register_prepare", utxo["txid"], utxo["vout"], [service()], k["owner"],
                  k["operator"]["public"], k["voting"], 0, k["payout"], next(fee))
    tx = raw(prep["tx"])
    payload = tx[payload_start(tx):]
    # nVersion, nType, nMode, then collateralOutpoint; vchSig (empty) last.
    assert payload[6:38] == bytes.fromhex(utxo["txid"])[::-1] and payload[-1] == 0
    unsigned = payload[:6] + bytes.fromhex(funding_txid)[::-1] + (0).to_bytes(4, "little") + payload[42:-1]
    message = prep["signMessage"].rsplit("|", 1)[0] + "|" + regtest.dsha256(unsigned)[::-1].hex()
    sig = base64.b64decode(r.call("signmessage", collateral, message))
    reg = signed(r, set_payload(tx, unsigned + regtest.compact_size(len(sig)) + sig).hex())
    out.append(record(r, "collateral_same_block", "collateral created in the same block",
                      [raw(funding), raw(reg)], CHECK,
                      "the ProRegTx's external collateral is an output of the block's previous transaction"))

    # A collateral spent by an ordinary transaction in the block of a ProUpServTx for it.
    mn = mns["mn2"]
    spend = r.call("createrawtransaction", [{"txid": mn["collateral"][0], "vout": mn["collateral"][1]}],
                   {r.call("getnewaddress"): 999.99})
    spend = signed(r, spend)
    ups = r.call("protx", "update_service", mn["protx"], [service()], mn["operator"]["secret"], "", next(fee), False)
    out.append(record(r, "collateral_spent_with_proupserv", "collateral spent in the block of a ProUpServTx",
                      [raw(spend), raw(ups)], LATER, "an ordinary spend of the collateral, then a ProUpServTx"))

    # A replacement ProRegTx: a new registration for mn2's collateral.
    k = r.keys()
    prep = r.call("protx", "register_prepare", mn["collateral"][0], mn["collateral"][1], [service()], k["owner"],
                  k["operator"]["public"], k["voting"], 0, k["payout"], next(fee))
    sig = base64.b64decode(r.call("signmessage", prep["collateralAddress"], prep["signMessage"]))
    tx = raw(prep["tx"])
    payload = tx[payload_start(tx):]
    reg = signed(r, set_payload(tx, payload[:-1] + regtest.compact_size(len(sig)) + sig).hex())
    out.append(record(r, "replacement_proregtx", "replacement ProRegTx", [raw(reg)], LATER,
                      "a ProRegTx for the collateral of a registered masternode"))

    # A ProUpServTx with an out-of-range nType (the payload's byte after its version).
    evo = mns["evo"]
    fee_evo = next(fee)
    for p2p, https in platform_addresses("127.0.0.1"):
        try:
            ups = raw(r.call("protx", "update_service_evo", evo["protx"], [service()], evo["operator"]["secret"],
                             "33" * 20, p2p, https, "", fee_evo, False))
            break
        except RuntimeError as e:
            if "platform" not in str(e):
                raise
    j = payload_start(ups)
    ups = bytearray(ups)
    assert int.from_bytes(ups[j + 2:j + 4], "little") == 1, "an evonode's ProUpServTx"
    ups[j + 2:j + 4] = (2).to_bytes(2, "little")
    out.append(record(r, "proupserv_bad_ntype", "ProUpServTx with out-of-range nType", [bytes(ups)], CHECK,
                      "nType 2 in an evonode's ProUpServTx (no MnType 2)"))

    # A ProUpRevTx, then a ProUpServTx for the same masternode.
    mn = mns["mn1"]
    rev = r.call("protx", "revoke", mn["protx"], mn["operator"]["secret"], 0, next(fee), False)
    ups = r.call("protx", "update_service", mn["protx"], [service()], mn["operator"]["secret"], "", next(fee), False)
    out.append(record(r, "prouprev_then_proupserv", "ProUpRevTx then ProUpServTx in one block",
                      [raw(rev), raw(ups)], LATER, "the update follows the revocation in BuildNewListFromBlock"))

    out += qc_cases(r)
    return out


def qc_cases(r):
    """Quorum commitments: Core's next block in a DKG mining window carries null ones;
    their quorumHash is replaced."""
    out = []
    for _ in range(48):
        block = r.chain.next_block()
        header, mined = regtest.split_block(block)
        qcs = [i for i, tx in enumerate(mined) if int.from_bytes(tx[2:4], "little") == 6]
        if qcs:
            break
        r.chain.mine(1)
    else:
        raise RuntimeError("no mining window")
    i = qcs[0]
    qc = mined[i]
    j = payload_start(qc)
    # CFinalCommitmentTxPayload: nVersion u16, nHeight u32, then the commitment: nVersion u16,
    # llmqType u8, quorumHash.
    hash_at = j + 2 + 4 + 2 + 1
    stale = bytes.fromhex(r.node.call("getblockhash", 1))[::-1]
    # Core's own block for this height, invalidated: in the block index, beyond the tip.
    tip = r.tip()
    beyond = next(t["hash"] for t in r.node.call("getchaintips") if t["status"] == "invalid" and t["height"] == tip + 1)
    for name, quorum_hash, operator in (
        ("qc_wrong_quorum_hash", bytes(range(32)), "QcTx with a wrong quorum hash"),
        ("qc_beyond_tip", bytes.fromhex(beyond)[::-1], "QcTx naming a block beyond the tip"),
        ("qc_old_quorum_hash", stale, "QcTx naming an old block (not the one its height computes)"),
    ):
        mutated = bytearray(qc)
        mutated[hash_at:hash_at + 32] = quorum_hash
        txs = mined[:i] + [bytes(mutated)] + mined[i + 1:]
        crafted = regtest.join_block(header, txs)
        reason = r.chain.propose(crafted)
        # An old block passes the check loop's lookup; the quorum block processor compares it
        # with the hash the height and quorum index give.
        stage = LATER if name == "qc_old_quorum_hash" else CHECK
        out.append({"name": name, "operator": operator, "v24": r.v24, "height": r.tip() + 1,
                    "context": r.context([]), "block": crafted.hex(), "core_reason": reason, "stage": stage,
                    "note": "a null commitment of Core's block with quorumHash replaced"})
        print(f"{name}: {reason}", file=sys.stderr, flush=True)
    # A negative computed height: payload nHeight 0 (Core checks it against the block's).
    mutated = bytearray(qc)
    mutated[j + 2:j + 6] = (0).to_bytes(4, "little")
    txs = mined[:i] + [bytes(mutated)] + mined[i + 1:]
    crafted = regtest.join_block(header, txs)
    reason = r.chain.propose(crafted)
    out.append({"name": "qc_bad_height", "operator": "QcTx with a wrong height", "v24": r.v24,
                "height": r.tip() + 1, "context": r.context([]), "block": crafted.hex(), "core_reason": reason,
                "stage": CHECK, "note": "a null commitment of Core's block with payload nHeight 0"})
    print(f"qc_bad_height: {reason}", file=sys.stderr, flush=True)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--dashd", required=True)
    ap.add_argument("--pin", required=True)
    ap.add_argument("--datadir", required=True)
    ap.add_argument("--port", type=int, default=29400)
    args = ap.parse_args()
    version = None
    cases = []
    chains = {}
    for v24 in (False, True):
        r = Regtest(args.dashd, Path(args.datadir) / ("v24" if v24 else "default"), args.port + (10 if v24 else 0),
                    v24)
        try:
            version = r.node.call("getnetworkinfo")["subversion"]
            if v24:
                # No registered masternodes: ProTxs are version 3 (ExtAddr) here, whose states
                # dml-listdiff-check.py does not encode, and this chain's case needs none.
                cases += cases_common(r)
            else:
                mns = setup(r)
                cases += cases_common(r)
                cases += cases_pre_v24(r, mns)
            tip = r.tip()
            chains["v24" if v24 else "default"] = "".join(r.chain.header_hex(h) for h in range(tip + 1))
        finally:
            r.node.stop()
    json.dump({"pin": args.pin, "dashd": version, "chains": chains, "cases": cases}, sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    main()
