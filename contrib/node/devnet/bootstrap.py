#!/usr/bin/env -S python3 -I -B
"""Bootstrap the Core-only devnet and judge its rehearsal observation stream."""

from concurrent.futures import ThreadPoolExecutor
from decimal import Decimal
import hashlib
import json
import os
from pathlib import Path
import secrets
import struct
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen
import lifecycle as life

SLOTS = [*(f"reg{i}" for i in range(20)), *(f"evo{i}" for i in range(12))]
SPORKS = ("SPORK_17_QUORUM_DKG_ENABLED", "SPORK_19_CHAINLOCKS_ENABLED",
          "SPORK_21_QUORUM_ALL_CONNECTED", "SPORK_23_QUORUM_POSE", "SPORK_2_INSTANTSEND_ENABLED")
TYPES = {q[0]: q[1] for q in gen.QUORUMS}
SIZES = {101: 12, 105: 8, 107: 12}
COLLATERAL = 20 * 1000 + 12 * 4000
FEES = 1


def require(condition, message):
    if not condition:
        raise ValueError(message)


def operator(seed, slot, fromsecret):
    for counter in range(256):
        suffix = counter.to_bytes(4, "big") if counter else b""
        key = hashlib.sha256(seed + slot.encode() + suffix).hexdigest()
        public = fromsecret(key)
        if public:
            return key, public, counter
    raise ValueError(f"no accepted operator key for {slot}")


def funding_height(start, consensus):
    # Conservatively allow the miner only one fifth of the fixed 5 DASH base.
    per_block = consensus["highsubsidyfactor"]
    blocks = (COLLATERAL + FEES + per_block - 1) // per_block
    target = start + blocks + 100
    require(target <= consensus["highsubsidyblocks"], "high-subsidy window too short for mature funding")
    return target


def ready_guard(rows, registrations):
    require(set(rows) == set(registrations) == set(SLOTS), "incomplete READY census")
    for slot, row in rows.items():
        require(row["state"] == "READY" and row["proTxHash"] == registrations[slot],
                f"{slot} is not READY for its registered ProTx")


def check_coverage(heights, tip):
    require(tip >= 1 and heights == list(range(1, tip + 1)),
            f"expected one Block at every height from 1 through {tip}")


def hash256(raw):
    return hashlib.sha256(hashlib.sha256(raw).digest()).digest()[::-1].hex()


def mnhf_tx(bit, quorum, sig):
    payload = bytes([1, bit]) + bytes.fromhex(quorum)[::-1] + sig
    require(len(payload) == 130, "invalid EHF payload length")
    return struct.pack("<HHBBIB", 3, 7, 0, 0, 0, len(payload)) + payload


class Commitments:
    def __init__(self):
        self.finals = {k: [] for k in SIZES}
        self.nulls = {k: 0 for k in TYPES}

    def check(self, height, qc, info, evos):
        kind, qhash = qc["llmqType"], qc["quorumHash"]
        require(kind in TYPES, f"unexpected quorum type {kind} at {height}")
        if (qc["signersCount"] == qc["validMembersCount"] == 0 and
                all(int(qc[k], 16) == 0 for k in ("quorumPublicKey", "quorumVvecHash", "quorumSig", "membersSig"))):
            self.nulls[kind] += 1
            return
        require(kind in SIZES, f"non-null {TYPES[kind]} at {height}")
        quorum = info(kind, qhash)
        members = quorum["members"]
        ids = {m["proTxHash"] for m in members}
        require(quorum["quorumHash"] == qhash and quorum["type"] == TYPES[kind] and
                len(ids) == len(members) == qc["validMembersCount"] == SIZES[kind] and
                all(m["valid"] is True for m in members), f"incomplete or invalid {TYPES[kind]} at {height}")
        require(kind != 107 or ids <= evos, "platform quorum contains a non-EvoNode")
        require(not any(e["hash"] == qhash for e in self.finals[kind]), "duplicate final commitment")
        self.finals[kind].append({"height": height, "base": quorum["height"], "hash": qhash,
                                  "index": quorum["quorumIndex"], "valid": len(ids)})

    def complete(self):
        for kind, rows in self.finals.items():
            require(len(rows) >= 2, f"fewer than two {TYPES[kind]} commitments")
        cycles = {}
        for row in self.finals[105]:
            cycles.setdefault(row["base"] - row["index"], set()).add(row["index"])
        require(sum(indices == {0, 1} for indices in cycles.values()) >= 2,
                "fewer than two complete rotating cycles")

    def report(self):
        return {"finals": {TYPES[k]: v for k, v in self.finals.items()},
                "five_types": {TYPES[k]: {"null": self.nulls[k], "non_null": 0}
                               for k in TYPES if k not in SIZES}}


class Bootstrap:
    def __init__(self, directory):
        self.directory = directory
        self.state = life.state_at(directory)
        require(self.state["harness"] and list(self.state["slots"]) == ["node0", *SLOTS],
                "expected the 33-slot Core-only rehearsal")
        self.records = {"run_id": self.state["run_id"], "steps": {}}
        self.started = time.monotonic()
        self.keys = {}
        self.registrations = {}
        self.signals = {}
        self.commitments = Commitments()
        self.scanned = -1

    def step(self, name, fn):
        print(f"bootstrap: {name}", flush=True)
        self.records["current"] = name
        self.save()
        try:
            self.records["steps"][name] = fn()
        except Exception as exc:
            # A dash-cli argument can be an operator secret.
            reason = exc.stderr if isinstance(exc, subprocess.CalledProcessError) else str(exc)
            if isinstance(exc, subprocess.TimeoutExpired):
                reason = "command timed out"
            self.records["error"] = (reason or "command failed").strip()
            self.save()
            raise ValueError(f"{name}: {self.records['error']}") from None
        self.save()

    def save(self):
        self.records["elapsed_seconds"] = round(time.monotonic() - self.started, 2)
        self.records["scanned_through"] = self.scanned
        self.records.update(self.commitments.report())
        self.records["signals"] = self.signals
        path = self.directory / "bootstrap.json"
        tmp = path.with_suffix(".tmp")
        gen.write_json(tmp, self.records)
        tmp.replace(path)

    def rpc(self, *args, slot="node0"):
        return life.rpc(self.state, slot, *map(str, args))

    def data(self, *args, slot="node0"):
        return json.loads(self.rpc(*args, slot=slot))

    def each(self, slots, fn):
        with ThreadPoolExecutor(max_workers=8) as pool:
            return dict(zip(slots, pool.map(fn, slots), strict=True))

    def height(self):
        return int(self.rpc("getblockcount"))

    def sync(self, tip):
        slots = self.state["slots"]
        life.eventually(lambda: set(self.each(slots, lambda s: self.rpc("getbestblockhash", slot=s)).values())
                        == {tip}, "common tip " + tip)

    def inspect_block(self, block):
        block_hash = block["hash"]
        h = block["height"]
        require(h == self.scanned + 1 and (h == 0 or block["previousblockhash"] == self.last_hash),
                f"gap or fork at {h}")
        evos = {v for k, v in self.registrations.items() if k.startswith("evo")}
        for tx in block["tx"]:
            if tx["type"] == 6:
                self.commitments.check(h, tx["qcTx"]["commitment"],
                                       lambda k, q: self.data("quorum", "info", k, q), evos)
            if tx["type"] == 7:
                signal = tx["mnhfTx"]["signal"]
                self.signals[signal["versionBit"]] = {"height": h, "txid": tx["txid"],
                                                       "quorum": signal["quorumHash"]}
        self.scanned, self.last_hash = h, block_hash

    def mine(self, n):
        if n <= 0:
            return
        while n:
            count = min(n, 96)
            hashes = self.data("generatetoaddress", count, self.address)
            require(len(hashes) == count, f"mined {len(hashes)} of {count}")
            self.sync(hashes[-1])
            with ThreadPoolExecutor(max_workers=6) as pool:
                for block in pool.map(lambda h: self.data("getblock", h, 2), hashes):
                    self.inspect_block(block)
            n -= count

    def watch(self):
        self.address, = json.loads((self.directory / "nodes.json").read_text())["coinbase"]
        self.harness = self.state["project"] + "-harness"
        self.obs = self.directory / "observations/obs.jsonl"
        first = next((r for r in self.observations() if r["kind"] == "Watermark"), None)
        require(first and first["height"] == 1, "missing initial watcher watermark")
        for i in range(self.height() + 1):
            self.inspect_block(self.data("getblock", self.rpc("getblockhash", i), 2))
        return {"first_watermark": first["height"], "harness": self.harness}

    def observations(self):
        with self.obs.open("rb") as stream:
            for line in stream:
                if line.endswith(b"\n"):
                    yield json.loads(line)

    def buried(self):
        buried = {k: v for k, v in self.data("getblockchaininfo")["softforks"].items()
                  if v["type"] == "buried"}
        required = {"dip0001", "dip0003", "dip0008", "dip0020", "dip0024", "realloc", "v19",
                    "v20", "mn_rr", "withdrawals"}
        require(required <= buried.keys() and all(v["active"] and v["height"] <= 2 for v in buried.values())
                and all(buried[k]["height"] == 2 for k in required), "buried activation mismatch")
        return {k: v["height"] for k, v in buried.items()}

    def configure(self):
        seed = secrets.token_bytes(32)
        path = self.directory / "run-seed"
        path.write_text(seed.hex() + "\n")
        path.chmod(0o600)
        spork = self.rpc("getnewaddress")
        wif = self.rpc("dumpprivkey", spork)

        def fromsecret(key):
            try:
                return self.data("bls", "fromsecret", key)["public"]
            except subprocess.CalledProcessError as exc:
                if "must be a valid BLS secret key" in exc.stderr:
                    return None
                raise

        for slot in self.state["slots"]:
            extra = [f"sporkaddr={spork}"]
            if slot == "node0":
                extra.append(f"sporkkey={wif}")
            else:
                private, public, counter = operator(seed, slot, fromsecret)
                extra.append(f"masternodeblsprivkey={private}")
                entry = {"public": public, "counter": counter}
                if slot.startswith("evo"):
                    port = 22100 + 2 * int(slot[3:])
                    entry.update(node_id=hashlib.sha256(seed + b"platform/" + slot.encode()).hexdigest()[:40],
                                 p2p=port, https=port + 1)
                self.keys[slot] = entry
            conf = self.directory / "nodes" / slot / "dash.conf"
            conf.chmod(0o644)
            with conf.open("a") as f:
                f.write("\n".join(extra) + "\n")
            conf.chmod(0o444)
        life.docker("compose", "-p", self.state["project"], "-f", str(self.directory / "compose.yaml"),
                    "restart", "-t", "30", *self.state["slots"])
        life.check_peering(self.state, restarted=True)
        for i in range(self.scanned + 1, self.height() + 1):
            self.inspect_block(self.data("getblock", self.rpc("getblockhash", i), 2))
        return {"sporkaddr": spork, "restarted_slots": list(self.state["slots"]),
                "peering": "peering-restarted.json", "seed_sha256": hashlib.sha256(seed).hexdigest()}

    def fund(self):
        start = self.height()
        target = funding_height(start, self.state["consensus"])
        self.mine(target - start)
        utxos = [u for u in self.data("listunspent") if u["address"] == self.address and u["spendable"]]
        total = sum(Decimal(str(u["amount"])) for u in utxos)
        require(total >= COLLATERAL + FEES, f"only {total} DASH spendable at mining address")
        return {"height": target, "spendable": str(total), "collateral": COLLATERAL, "fee_reserve": FEES}

    def identities(self):
        evos = [k for k in self.keys.values() if "node_id" in k]
        require(len({k["public"] for k in self.keys.values()}) == 32 and
                len({k["node_id"] for k in evos}) == 12 and
                len({k[p] for k in evos for p in ("p2p", "https")}) == 24, "duplicate identity or port")
        return self.keys

    def register(self):
        for slot, entry in self.keys.items():
            evo = slot.startswith("evo")
            service = self.state["slots"][slot] + f":{gen.P2P}"
            owner = self.rpc("getnewaddress")
            args = [self.rpc("getnewaddress"), service, owner, entry["public"], owner, 0, self.address]
            if evo:
                args.extend([entry["node_id"], entry["p2p"], entry["https"]])
            self.registrations[slot] = self.rpc("protx", "register_fund_evo" if evo else "register_fund",
                                                *args, self.address)
            self.mine(1)
        self.mine(2)
        registrations = {}
        for slot, txid in self.registrations.items():
            row = self.data("protx", "info", txid)
            service = self.state["slots"][slot] + f":{gen.P2P}"
            require(row["state"]["addresses"]["core_p2p"] == [service] and
                    row["confirmations"] >= 2, f"registration mismatch for {slot}")
            registrations[slot] = {"txid": txid, "service": service,
                                   "height": row["state"]["registeredHeight"]}
        return registrations

    def ready(self):
        # The rotating quorum selects quarters at work blocks three cycles back.
        base = ((self.height() + 8 + 47) // 48 + 3) * 48
        self.mine(base - 1 - self.height())
        def census():
            rows = self.each(SLOTS, lambda s: self.data("masternode", "status", slot=s))
            return rows if all(r["state"] == "READY" for r in rows.values()) else None
        rows = life.eventually(census, "all masternodes READY", timeout=360)
        ready_guard(rows, self.registrations)
        values = self.each(self.state["slots"], lambda s: self.data("spork", "show", slot=s))
        require(all(row[k] > time.time() for row in values.values() for k in SPORKS),
                "spork already active before READY")
        return {s: {"state": r["state"], "proTxHash": r["proTxHash"]} for s, r in rows.items()}

    def sporks(self):
        ready_guard(self.records["steps"].get("ready", {}), self.registrations)
        for name in SPORKS:
            require(self.rpc("sporkupdate", name, 0) == "success", f"sporkupdate failed for {name}")
        def propagated():
            rows = self.each(self.state["slots"], lambda s: self.data("spork", "show", slot=s))
            return rows if all(r[k] == 0 for r in rows.values() for k in SPORKS) else None
        return life.eventually(propagated, "sporks on every slot")

    def phase(self, kind, qhash, phase):
        size = SIZES[kind]
        name = TYPES[kind]
        field = {2: "receivedContributions", 5: "receivedPrematureCommitments"}.get(phase)
        by_txid = {v: s for s, v in self.registrations.items()}
        def settled():
            rows = self.each(SLOTS, lambda s: self.data("quorum", "dkgstatus", slot=s))
            count = 0
            probes = set()
            for slot, row in rows.items():
                for session in row["session"]:
                    status = session["status"]
                    if session["llmqType"] != name or status["quorumHash"] != qhash:
                        continue
                    if status["phase"] != phase or (field and status[field] < size):
                        return False
                    if phase == 1:
                        peers = next((c["quorumConnections"] for c in row["quorumConnections"]
                                      if c.get("quorumHash") == qhash and c["llmqType"] == name), [])
                        if sum(p["connected"] for p in peers) < size - 1:
                            return False
                        probes.update((by_txid[p["proTxHash"]], self.registrations[slot])
                                      for p in peers if not p["outbound"])
                    count += 1
            if count != size:
                return False
            def recent(probe):
                peer, txid = probe
                meta = self.data("protx", "info", txid, slot=peer)["metaInfo"]
                return meta["lastOutboundSuccessElapsed"] < 55 * 60
            return all(self.each(probes, recent).values())
        life.eventually(settled, f"{name} {qhash} phase {phase}", timeout=240)

    def dkgs(self):
        # Core v24.0.2 test_framework.py mine_quorum / mine_cycle_quorum:
        # stagger both indices by one block, sync the network at each phase.
        for cycle in range(2):
            self.mine(1)
            base = self.height()
            require(base % 48 == 0, "rotating DKG not at cycle boundary")
            starts = {}
            for offset in range(12):
                if offset:
                    self.mine(1)
                if offset == 0:
                    starts[0] = self.last_hash
                if offset == 1:
                    starts[1] = self.last_hash
                phase = 1 + offset // 2
                if offset % 2 == 0:
                    for kind in (101, 107, 105):
                        self.phase(kind, starts[0], phase)
                else:
                    self.phase(105, starts[1], phase)
                if offset in (10, 11):
                    expected = {(101, starts[0]), (107, starts[0])} if offset == 10 else {
                        (105, starts[0]), (105, starts[1])}
                    def minable():
                        qcs = self.data("quorum", "dkgstatus")["minableCommitments"]
                        return expected <= {(q["llmqType"], q["quorumHash"]) for q in qcs
                                            if q["validMembersCount"] == SIZES.get(q["llmqType"])}
                    life.eventually(minable, f"miner commitments at {base}+{offset}")
            self.mine(1)
            if cycle == 0:
                # Pace the intervening 24-block nonrotating round as well.
                self.mine(12)
                qhash = self.last_hash
                for phase in range(1, 7):
                    if phase > 1:
                        self.mine(2)
                    for kind in (101, 107):
                        self.phase(kind, qhash, phase)
                self.mine(13)
        self.commitments.complete()
        return self.commitments.report()["finals"]

    def activate(self, name, bit):
        if bit == 14:
            request = hash256(b"\x04mnhf" + struct.pack("<Q", bit))
            quorum = self.data("quorum", "selectquorum", 101, request)["quorumHash"]
            msg = hash256(mnhf_tx(bit, quorum, bytes(96)))
            members = self.data("quorum", "info", 101, quorum)["members"]
            by_txid = {v: k for k, v in self.registrations.items()}
            slots = [by_txid[m["proTxHash"]] for m in members if m["valid"]]
            self.each(slots, lambda s: self.rpc("quorum", "sign", 101, request, msg, quorum, slot=s))
            sig = life.eventually(lambda: self.data("quorum", "getrecsig", 101, request, msg,
                                                    slot=slots[0]), "EHF recovered signature")
            txid = self.rpc("sendrawtransaction", mnhf_tx(bit, quorum, bytes.fromhex(sig["sig"])).hex(), 0)
            self.mine(1)
            require(self.signals.get(bit, {}).get("txid") == txid, "manual EHF signal not mined")
        else:
            if bit not in self.signals:
                def pending():
                    return any(self.data("getrawtransaction", tx, 1).get("mnhfTx", {}).get("signal", {})
                               .get("versionBit") == bit for tx in self.data("getrawmempool"))
                life.eventually(pending, "automatic v24 EHF in miner mempool")
                self.mine(1)
            require(bit in self.signals, "automatic EHF signal not mined")
        signal = self.signals[bit]["height"]
        # v24.0.2 chainparams.cpp:595 and versionbits.cpp:64-69: first
        # eligible period end must reach both the EHF signal and height 2018.
        expected = (max(signal, 2018) // 120 + 3) * 120
        require(expected >= self.height(), f"{name} activation window already past")
        self.mine(expected - self.height())
        info = self.data("getblockchaininfo")["softforks"][name]
        require(info["bip9"]["status"] == "active" and info["height"] == expected, f"{name}: {info}")
        return {"path": "Core automatic" if bit == 13 else "manual create_mnehf", "height": expected,
                "signal": self.signals[bit], "period": 120}

    def judge(self):
        obs_dir = self.obs.parent
        os.close(os.open(obs_dir / "watch.events", os.O_WRONLY | os.O_NONBLOCK))
        status = obs_dir / "watch.exit"
        code = life.eventually(lambda: status.exists() and status.read_text().strip(), "watcher EOF", timeout=300)
        require(code == "0", f"watcher exited {code}; see observations/watch.log")
        judge = subprocess.run(["docker", "exec", self.harness, "mixnet-judge", "/observations/obs.jsonl",
                                "--plan", "/plan.json", "--gate", "rehearsal"], text=True, capture_output=True)
        (self.directory / "judge.json").write_text(judge.stdout)
        require(judge.returncode == 0, f"judge exited {judge.returncode}: {judge.stderr}")
        report = json.loads(judge.stdout)["report"]
        require(report["passed"], "rehearsal failed")
        first = next(self.observations())
        require(first["kind"] == "Run" and first["run_kind"] == "devnet-rehearsal", "wrong Run record")
        heights = [r["height"] for r in self.observations() if r["kind"] == "Block"]
        check_coverage(heights, self.height())
        self.records["judge"] = {"exit": 0, "gate": "rehearsal", "passed": True,
                                 "range": [heights[0], heights[-1]], "run_kind": first["run_kind"]}
        return self.records["judge"]

    def run(self):
        for name, fn in (("watch", self.watch), ("buried", self.buried), ("restart", self.configure),
                         ("fund", self.fund), ("keys", self.identities), ("registrations", self.register),
                         ("ready", self.ready), ("sporks", self.sporks), ("dkgs", self.dkgs),
                         ("v24", lambda: self.activate("v24", 13)),
                         ("evo_shares", lambda: self.activate("evo_shares", 14)), ("judge", self.judge)):
            self.step(name, fn)


if __name__ == "__main__":
    try:
        Bootstrap(Path(os.environ["DEVNET_RUN_DIR"])).run()
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as exc:
        sys.exit(str(exc))
