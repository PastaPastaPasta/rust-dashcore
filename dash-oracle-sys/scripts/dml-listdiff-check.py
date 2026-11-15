#!/usr/bin/env python3
"""Check oracle_dml_apply and oracle_dml_block against `protx listdiff` on sampled blocks
(node-program O2a, O2a-EV).

For each sampled height h this reads, from a reference dashd over its RPC (read-only
calls, one at a time: getblockcount, getblockhash, getblock, getblockheader and
`protx listdiff`, which A1/ORACLE.md list as pure reads):
- block h and the headers from the oldest quorum base block its commitments name up
  to h - 1 (the context the shim needs);
- the masternode list after h - 1, as `protx listdiff <height 1> <h - 1>` prints it,
  re-encoded the way evodb stores it;
- for every non-null quorum commitment in block h, the quorum's members;
- the coins the block's ProRegTxs name as external collateral and the collaterals of the
  masternodes its ProUpRegTxs update (getrawtransaction), in a
  coins view whose best block is the tip, so Core runs the collateral re-check it does
  only when connecting at the tip;
- `protx listdiff <h - 1> <h>`, the expected answer;
- block h's CbTx merkleRootMNList and, from DIP3 enforcement, the masternodes of the parent's
  list whose payout script block h's coinbase pays (the payee candidates).
It pipes each sample to the `dml_listdiff` example (dash-oracle-sys/examples), which
runs oracle_dml_apply and oracle_dml_block on every pin and compares the result with the
RPC's listdiff, the payee with the candidates and the new list's SML root with the CbTx's,
and records one result line per sample and pin.

What the RPC does not print is derived, and checked end to end by the comparison:
- confirmedHash: Core sets it to the hash of block registeredHeight + 15 (mainnet's
  nMasternodeMinimumConfirmations) while connecting block registeredHeight + 16;
- internal ids and nTotalRegisteredCount: any unique ids give the same diff;
- quorum members of non-rotated quorums: CalculateQuorum() over the list at the quorum
  base block, or once V20 is active after it at the work block (base - 8) (only evonodes
  for the platform type from V19), with the modifier SerializeHash(llmqType, base block
  hash), or once V20 is active after the work block SerializeHash(llmqType, work height,
  its CbTx chainlock signature), or the work block's hash when that is null. This script
  computes them (a harness-only third implementation). oracle_dml_block does not
  authenticate them: Core's check loop verifies a commitment without its signatures
  (CheckLLMQCommitment calls Verify(..., false); the quorum block processor, which
  dml_block leaves out, checks them). Wrong members can show up as a PoSe mismatch only
  for a punishing commitment (HandleQuorumCommitment punishes the members whose
  validMembers bit is clear); the signer keys of any other commitment are authenticated
  only by leg qc_sigs: the checker runs oracle_verify_commitment with signatures on every
  non-null commitment, and again with a signer's key replaced by another member's, which
  must be rejected (leg qc_control).

The script refuses what it cannot represent: masternode states other than versions 1 and 2
(legacy and basic BLS; no ExtAddr), non-null commitments of a rotated quorum (their members
come from quorum snapshots) and unknown LLMQ types.

Usage:
  dml-listdiff-check.py --checker <target>/debug/examples/dml_listdiff --count 1000 \
      --seed 1 --out results.jsonl [--fail-dir DIR] [--net mainnet]
      [--heights-file FILE] [--resume] [--save-samples FILE.gz]
  dml-listdiff-check.py --checker <dml_listdiff> --replay FILE.gz --out results.jsonl

--heights-file takes one height per line, or a previous run's result lines; --resume
appends to --out and skips heights it already holds for every pin. --save-samples keeps
every fetched sample (gzip JSON lines), and --replay FILE checks those samples again
without the RPC, e.g. on newly built archives.
"""

import argparse
import base64
import gzip
import hashlib
import ipaddress
import json
import random
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# The oracle pins this script runs on; the checker writes one result line per sample and pin.
PINS = ("v24.0",)

NETWORKS = {
    # From Core's chainparams (oracle_chainparams_json at the pin).
    "mainnet": {
        "chain": "main",
        "rpc_port": 27998,
        "cookie": "/work/chains/mainnet/rpc.cookie",
        "dip3_height": 1028160,
        "dip3_enforcement_height": 1047200,
        "v19_height": 1899072,
        "v20_height": 1987776,
        "min_confirmations": 15,
        "pubkey_prefix": 76,
        "script_prefix": 16,
        # llmqType: (size, dkgInterval) of the non-rotated types
        "llmqs": {1: (50, 24), 2: (400, 288), 3: (400, 576), 4: (100, 24)},
        "platform_llmq": 4,
        # llmqType: dkgInterval of the rotated types (null commitments only)
        "rotated_llmqs": {5: 288},
    },
}

B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


class Rpc:
    """JSON-RPC over HTTP with the node's cookie; re-reads the cookie on auth errors
    (the oracle services restart and rotate it)."""

    def __init__(self, port, cookie_path):
        self.url = f"http://127.0.0.1:{port}/"
        self.cookie_path = cookie_path
        self.auth = None

    def call(self, method, *params):
        for attempt in range(5):
            if self.auth is None:
                cookie = Path(self.cookie_path).read_text().strip()
                self.auth = base64.b64encode(cookie.encode()).decode()
            body = json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode()
            req = urllib.request.Request(self.url, data=body, headers={"Authorization": f"Basic {self.auth}"})
            try:
                with urllib.request.urlopen(req, timeout=600) as resp:
                    reply = json.load(resp)
            except urllib.error.HTTPError as e:
                if e.code == 401:
                    self.auth = None
                    continue
                reply = json.load(e)
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                self.auth = None
                time.sleep(2 * (attempt + 1))
                continue
            if reply.get("error"):
                raise RuntimeError(f"{method}{params}: {reply['error']}")
            return reply["result"]
        raise RuntimeError(f"{method}{params}: no answer")


def sha256(b):
    return hashlib.sha256(b).digest()


def dsha256(b):
    return sha256(sha256(b))


def internal(hex_hash):
    """A uint256 printed by the RPC (big-endian hex) in Core's internal byte order."""
    return bytes.fromhex(hex_hash)[::-1]


def compact_size(n):
    if n < 0xFD:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + n.to_bytes(2, "little")
    if n <= 0xFFFFFFFF:
        return b"\xfe" + n.to_bytes(4, "little")
    return b"\xff" + n.to_bytes(8, "little")


def varint(n):
    """Core's VARINT (VarIntMode::DEFAULT)."""
    out = []
    while True:
        out.append((n & 0x7F) | (0x80 if out else 0))
        if n <= 0x7F:
            break
        n = (n >> 7) - 1
    return bytes(reversed(out))


def i32(n):
    return n.to_bytes(4, "little", signed=True)


def u16(n):
    return n.to_bytes(2, "little")


def u32(n):
    return n.to_bytes(4, "little")


def b58decode_check(s):
    n = 0
    for c in s:
        n = n * 58 + B58.index(c)
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    raw = b"\x00" * (len(s) - len(s.lstrip("1"))) + raw
    payload, checksum = raw[:-4], raw[-4:]
    if dsha256(payload)[:4] != checksum:
        raise ValueError(f"bad checksum in {s}")
    return payload[0], payload[1:]


def key_id(address, net):
    version, h160 = b58decode_check(address)
    if version != net["pubkey_prefix"] or len(h160) != 20:
        raise ValueError(f"{address} is not a P2PKH address")
    return h160


def script(address, net):
    version, h160 = b58decode_check(address)
    if version == net["pubkey_prefix"]:
        return b"\x76\xa9\x14" + h160 + b"\x88\xac"
    if version == net["script_prefix"]:
        return b"\xa9\x14" + h160 + b"\x87"
    raise ValueError(f"unknown address version in {address}")


def cscript(b):
    return compact_size(len(b)) + b


def cservice(entries):
    """MnNetInfo below ExtAddr: one CService (16-byte address, big-endian port)."""
    if not entries:
        return bytes(18)
    if len(entries) != 1:
        raise ValueError(f"more than one core_p2p address: {entries}")
    host, port = entries[0].rsplit(":", 1)
    if host.startswith("["):
        addr = ipaddress.IPv6Address(host[1:-1]).packed
    else:
        addr = bytes(10) + b"\xff\xff" + bytes(int(x) for x in host.split("."))
    return addr + int(port).to_bytes(2, "big")


def first_port(entries):
    """The port of an address list's first entry (a platform port before ExtAddr), 0 without one."""
    return int(entries[0].rsplit(":", 1)[1]) if entries else 0


class Chain:
    def __init__(self, rpc, net):
        self.rpc = rpc
        self.net = net
        self.hashes = {}

    def block_hash(self, height):
        if height not in self.hashes:
            self.hashes[height] = self.rpc.call("getblockhash", height)
        return self.hashes[height]

    def confirmed_hash(self, registered_height, list_height):
        """confirmedHash of a masternode in the list after block list_height."""
        if registered_height + self.net["min_confirmations"] + 1 > list_height:
            return bytes(32)
        return internal(self.block_hash(registered_height + self.net["min_confirmations"]))

    def headers_hex(self, first, end):
        """The serialized headers at heights first .. end - 1."""
        return "".join(self.rpc.call("getblockheader", self.block_hash(h), False) for h in range(first, end))

    def list_json(self, height):
        """The masternodes after block height, as protx listdiff prints them."""
        if height < self.net["dip3_height"]:
            return None
        diff = self.rpc.call("protx", "listdiff", 1, height)
        assert not diff["removedMNs"] and not diff["updatedMNs"], "a diff from an empty list"
        return diff["addedMNs"]


def by_registration(mns):
    """The order internal ids are assigned in (any unique ids give the same diff)."""
    return sorted(mns, key=lambda m: (m["state"]["registeredHeight"], m["proTxHash"]))


def quorum_base_height(payload, interval):
    """The quorum base block a commitment payload names (RebuildListFromBlock)."""
    return payload["height"] - payload["height"] % interval + payload["commitment"]["quorumIndex"]


MN_TYPES = {"Regular": 0, "Evo": 1}


def encode_dmn(mn, internal_id, list_height, chain):
    net = chain.net
    st = mn["state"]
    if st["version"] not in (1, 2) or mn["type"] not in MN_TYPES:
        raise ValueError(f"unsupported masternode {mn['proTxHash']}: version {st['version']}, {mn['type']}")
    protx = internal(mn["proTxHash"])
    confirmed = chain.confirmed_hash(st["registeredHeight"], list_height)
    with_protx = sha256(protx + confirmed) if confirmed != bytes(32) else bytes(32)
    operator_reward = round(mn["operatorReward"] * 100)
    addresses = st.get("addresses", {})
    pubkey_operator = bytes.fromhex(st["pubKeyOperator"])
    if len(pubkey_operator) != 48:
        raise ValueError("pubKeyOperator is not 48 bytes")
    state = b"".join(
        [
            i32(st["version"]),
            i32(st["registeredHeight"]),
            i32(st["lastPaidHeight"]),
            i32(st["consecutivePayments"]),
            i32(st["PoSePenalty"]),
            i32(st["PoSeRevivedHeight"]),
            i32(st["PoSeBanHeight"]),
            u16(st["revocationReason"]),
            confirmed,
            with_protx,
            key_id(st["ownerAddress"], net),
            pubkey_operator,
            key_id(st["votingAddress"], net),
            cservice(addresses.get("core_p2p", [])),
            cscript(script(st["payoutAddress"], net)),
            cscript(script(st["operatorPayoutAddress"], net) if "operatorPayoutAddress" in st else b""),
            internal(st["platformNodeID"]) if "platformNodeID" in st else bytes(20),
            u16(first_port(addresses.get("platform_p2p", []))),
            u16(first_port(addresses.get("platform_https", []))),
        ]
    )
    return b"".join(
        [
            protx,
            varint(internal_id),
            internal(mn["collateralHash"]),
            u32(mn["collateralIndex"]),
            u16(operator_reward),
            state,
            u16(MN_TYPES[mn["type"]]),
        ]
    )


def encode_list(mns, height, chain):
    """A CDeterministicMNList as evodb stores it (the default list before DIP3)."""
    if mns is None:
        return bytes(32) + i32(-1) + u32(0) + compact_size(0)
    mns = by_registration(mns)
    out = [internal(chain.block_hash(height)), i32(height), u32(len(mns)), compact_size(len(mns))]
    out += [encode_dmn(mn, i, height, chain) for i, mn in enumerate(mns)]
    return b"".join(out)


def cbtx(chain, height):
    """The CbTx payload of block height, as getblock 2 prints it."""
    return chain.rpc.call("getblock", chain.block_hash(height), 2)["tx"][0]["cbTx"]


def hash_modifier(chain, llmq_type, base_height):
    """GetHashModifier() for a non-rotated quorum."""
    net = chain.net
    work = base_height - 8  # llmq::WORK_DIFF_DEPTH
    if work + 1 < net["v20_height"]:
        return dsha256(bytes([llmq_type]) + internal(chain.block_hash(base_height)))
    # GetNonNullCoinbaseChainlock(): V20 active at the work block, a version 3 CbTx at its
    # height with a non-null signature and bestCLHeightDiff below the height.
    if work >= net["v20_height"]:
        cb = cbtx(chain, work)
        sig = bytes.fromhex(cb.get("bestCLSignature", ""))
        if cb["version"] >= 3 and cb["height"] == work and sig.strip(b"\0") and cb["bestCLHeightDiff"] < work:
            return dsha256(bytes([llmq_type]) + i32(work) + sig)
    return dsha256(bytes([llmq_type]) + internal(chain.block_hash(work)))


def member_list_height(chain, base_height):
    """The block whose list GetAllQuorumMembers() reads for a non-rotated quorum: the work
    block (base - 8) once V20 is active after the base block, else the base block."""
    return base_height - 8 if base_height + 1 >= chain.net["v20_height"] else base_height


def quorum_members(chain, llmq_type, base_height):
    """CalculateQuorum() for a non-rotated quorum: valid, confirmed masternodes of the list
    at member_list_height() (evonodes only for the platform type once V19 is active after
    the base block), by descending sha256(sha256(proTxHash, confirmedHash) || modifier)."""
    net = chain.net
    size, _ = net["llmqs"][llmq_type]
    evo_only = llmq_type == net["platform_llmq"] and base_height + 1 >= net["v19_height"]
    modifier = hash_modifier(chain, llmq_type, base_height)
    list_height = member_list_height(chain, base_height)
    mns = chain.list_json(list_height) or []
    scored = []
    for mn in mns:
        st = mn["state"]
        if st["PoSeBanHeight"] != -1 or (evo_only and mn["type"] != "Evo"):
            continue
        confirmed = chain.confirmed_hash(st["registeredHeight"], list_height)
        if confirmed == bytes(32):
            continue
        protx = internal(mn["proTxHash"])
        score = int.from_bytes(sha256(sha256(protx + confirmed) + modifier), "little")
        collateral = (internal(mn["collateralHash"]), mn["collateralIndex"])
        scored.append((score, collateral, mn))
    scored.sort(key=lambda s: (s[0], s[1]), reverse=True)
    ids = {m["proTxHash"]: i for i, m in enumerate(by_registration(mns))}
    return [encode_dmn(mn, ids[mn["proTxHash"]], list_height, chain).hex() for _, _, mn in scored[:size]]


def coin_of(chain, txid, vout):
    """The coin txid:vout as a context coin record (getrawtransaction, txindex)."""
    tx = chain.rpc.call("getrawtransaction", txid, True)
    out = tx["vout"][vout]
    return {
        "txid": txid,
        "vout": vout,
        "height": tx["height"],
        "coinbase": "coinbase" in tx["vin"][0],
        "amount": out["valueSat"],
        "script": out["scriptPubKey"]["hex"],
    }


def build_sample(chain, height):
    net = chain.net
    block_hash = chain.block_hash(height)
    verbose = chain.rpc.call("getblock", block_hash, 2)
    quorums = []
    first_height = height - 1
    stats = {"tx_types": {}, "commitments": 0, "punishing": 0, "external_collaterals": 0, "upreg_collaterals": 0}
    coins = []
    prev_mns = chain.list_json(height - 1)
    collaterals = {mn["proTxHash"]: (mn["collateralHash"], mn["collateralIndex"]) for mn in prev_mns or []}
    for tx in verbose["tx"]:
        t = str(tx.get("type", 0))
        stats["tx_types"][t] = stats["tx_types"].get(t, 0) + 1
        if tx.get("type") == 1 and int(tx["proRegTx"]["collateralHash"], 16) != 0:
            coins.append(coin_of(chain, tx["proRegTx"]["collateralHash"], tx["proRegTx"]["collateralIndex"]))
            stats["external_collaterals"] += 1
        if tx.get("type") == 3:
            # CheckProUpRegTx reads the masternode's collateral from the view (the payout
            # address may not be the collateral's); once per masternode.
            collateral = collaterals.pop(tx["proUpRegTx"]["proTxHash"], None)
            if collateral is not None:
                coins.append(coin_of(chain, *collateral))
                stats["upreg_collaterals"] += 1
        if tx.get("type") != 6:
            continue
        payload = tx["qcTx"]
        qc = payload["commitment"]
        llmq_type = qc["llmqType"]
        if llmq_type in net["llmqs"]:
            size, interval = net["llmqs"][llmq_type]
        elif llmq_type in net["rotated_llmqs"]:
            size, interval = None, net["rotated_llmqs"][llmq_type]
        else:
            raise ValueError(f"LLMQ type {llmq_type} is not supported")
        # The check loop looks up the base block of every commitment, null ones too.
        base_height = quorum_base_height(payload, interval)
        assert chain.block_hash(base_height) == qc["quorumHash"], "quorum base block"
        first_height = min(first_height, member_list_height(chain, base_height))
        if qc["signersCount"] == 0 and qc["validMembersCount"] == 0 and int(qc["quorumVvecHash"], 16) == 0:
            continue  # a null commitment (CFinalCommitment::IsNull(); the rebuild skips those)
        if size is None:
            raise ValueError(f"a non-null commitment of rotated LLMQ type {llmq_type}")
        stats["commitments"] += 1
        if qc["validMembersCount"] < size:
            stats["punishing"] += 1
        quorums.append(
            {
                "llmq_type": llmq_type,
                "quorum_hash": internal(qc["quorumHash"]).hex(),
                "members": quorum_members(chain, llmq_type, base_height),
            }
        )
    headers = chain.headers_hex(first_height, height)
    tip_header = chain.rpc.call("getblockheader", chain.block_hash(height - 1), True)
    coinbase = verbose["tx"][0]
    payees = None
    if height >= net["dip3_enforcement_height"]:
        paid = {out["scriptPubKey"]["hex"] for out in coinbase["vout"]}
        payees = [
            internal(mn["proTxHash"]).hex() for mn in prev_mns if script(mn["state"]["payoutAddress"], net).hex() in paid
        ]
    return {
        "chain": net["chain"],
        "height": height,
        "block": chain.rpc.call("getblock", block_hash, 0),
        "first_height": first_height,
        "headers": headers,
        "block_hash": internal(block_hash).hex(),
        "prev_list": encode_list(prev_mns, height - 1, chain).hex(),
        "quorums": quorums,
        # The scheme Core sets once the parent is the tip (V19 active for block h).
        "bls_legacy": height < net["v19_height"],
        # V24 is not started on mainnet in the oracle's range (unlisted deployments are
        # DEFINED too; V19, V20 and MN_RR are buried).
        "deployments": [[1, 0]],
        "coins": coins,
        "coins_at_tip": True,
        "adjusted_time": tip_header["time"],
        "expected": chain.rpc.call("protx", "listdiff", height - 1, height),
        "cbtx_mn_root": internal(coinbase["cbTx"]["merkleRootMNList"]).hex() if "cbTx" in coinbase else None,
        "payees": payees,
        "stats": stats,
    }


def replay(args):
    """Check saved samples (--save-samples) with --checker, without the RPC."""
    checker = subprocess.Popen([args.checker], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    totals = {}
    with gzip.open(args.replay, "rt") as samples, open(args.out, "w") as out:
        for n, line in enumerate(samples, 1):
            sample = json.loads(line)
            stats = sample.pop("stats")
            checker.stdin.write(json.dumps(sample) + "\n")
            checker.stdin.flush()
            for _ in PINS:
                record = json.loads(checker.stdout.readline())
                record["stats"] = stats
                out.write(json.dumps(record) + "\n")
                key = f"{record['pin']}:{record['result']}"
                totals[key] = totals.get(key, 0) + 1
            if n % 100 == 0:
                print(f"[{n}] height {sample['height']}: {totals}", file=sys.stderr, flush=True)
    checker.stdin.close()
    checker.wait()
    print(json.dumps(totals, sort_keys=True))
    return 0 if all(k.endswith(":match") for k in totals) else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--net", default="mainnet", choices=sorted(NETWORKS))
    ap.add_argument("--checker", required=True, help="the dml_listdiff example binary")
    ap.add_argument("--count", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--heights", help="comma-separated heights instead of a random sample")
    ap.add_argument("--max-height", type=int, help="highest height to sample (default: the node's block count)")
    ap.add_argument("--out", required=True, help="result JSON lines")
    ap.add_argument("--fail-dir", help="keep the samples of mismatches and errors here")
    ap.add_argument("--heights-file", help="heights to check: one per line, or a previous run's result lines")
    ap.add_argument("--resume", action="store_true", help="append to --out, skipping heights it holds for every pin")
    ap.add_argument("--save-samples", help="also append every sample to this gzip JSON-lines file")
    ap.add_argument("--replay", help="check the samples of a --save-samples file instead of fetching them")
    args = ap.parse_args()

    if args.replay:
        return replay(args)

    net = NETWORKS[args.net]
    chain = Chain(Rpc(net["rpc_port"], net["cookie"]), net)
    if args.heights:
        heights = sorted(int(h) for h in args.heights.split(","))
    elif args.heights_file:
        lines = [line.strip() for line in Path(args.heights_file).read_text().splitlines() if line.strip()]
        heights = sorted({json.loads(line)["height"] if line.startswith("{") else int(line) for line in lines})
    else:
        top = args.max_height or chain.rpc.call("getblockcount")
        heights = sorted(random.Random(args.seed).sample(range(net["dip3_height"], top + 1), args.count))
    fail_dir = Path(args.fail_dir) if args.fail_dir else None
    if fail_dir:
        fail_dir.mkdir(parents=True, exist_ok=True)

    done = {}
    if args.resume and Path(args.out).exists():
        for line in Path(args.out).read_text().splitlines():
            record = json.loads(line)
            if record.get("pin") in PINS:
                done.setdefault(record["height"], set()).add(record["pin"])
    checker = subprocess.Popen([args.checker], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    totals = {}
    with open(args.out, "a" if args.resume else "w") as out:
        for n, height in enumerate(heights, 1):
            if done.get(height) == set(PINS):
                continue
            try:
                sample = build_sample(chain, height)
            except Exception as e:  # recorded, never silently skipped
                record = {"height": height, "pin": "-", "result": "sample-error", "error": str(e)}
                out.write(json.dumps(record) + "\n")
                totals["sample-error"] = totals.get("sample-error", 0) + 1
                continue
            if args.save_samples:
                with gzip.open(args.save_samples, "at") as saved:
                    saved.write(json.dumps(sample) + "\n")
            stats = sample.pop("stats")
            checker.stdin.write(json.dumps(sample) + "\n")
            checker.stdin.flush()
            bad = False
            for _ in PINS:
                record = json.loads(checker.stdout.readline())
                record["stats"] = stats
                out.write(json.dumps(record) + "\n")
                key = f"{record['pin']}:{record['result']}"
                totals[key] = totals.get(key, 0) + 1
                bad |= record["result"] != "match"
            out.flush()
            if bad and fail_dir:
                (fail_dir / f"{height}.json").write_text(json.dumps(sample))
            if n % 25 == 0 or bad:
                print(f"[{n}/{len(heights)}] height {height}: {totals}", file=sys.stderr, flush=True)
    checker.stdin.close()
    checker.wait()
    print(json.dumps(totals, sort_keys=True))
    return 0 if all(k.endswith(":match") for k in totals) else 1


if __name__ == "__main__":
    sys.exit(main())
