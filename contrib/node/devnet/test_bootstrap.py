"""Run with python3 -I -B contrib/node/devnet/test_bootstrap.py."""

import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bootstrap as b
import gen
import lifecycle


class BootstrapTests(unittest.TestCase):
    def test_derivation_and_retry(self):
        seed = bytes(range(32))
        material = seed + b"reg0"
        first = hashlib.sha256(material).hexdigest()
        second = hashlib.sha256(material + bytes.fromhex("00000001")).hexdigest()
        accept = Mock(side_effect=[None, "public"])
        self.assertEqual(b.operator(seed, "reg0", accept), (second, "public", 1))
        self.assertEqual([c.args[0] for c in accept.call_args_list], [first, second])
        self.assertEqual(b.operator(seed, "reg0", lambda _: "pub"), (first, "pub", 0))
        self.assertNotEqual(b.operator(seed, "reg1", lambda _: "pub")[0], first)

    def test_harness_glibc(self):
        with patch.object(gen, "output", return_value="GLIBC_2.9 GLIBC_2.39"):
            gen.check_harness_glibc(Path("watch"))
        with patch.object(gen, "output", return_value="GLIBC_2.39 GLIBC_2.40"):
            with self.assertRaisesRegex(ValueError, "requires glibc 2.40"):
                gen.check_harness_glibc(Path("watch"))

    def test_harness_starts_between_connectivity_and_mining(self):
        slots = {"node0": "11.1.2.10", "reg0": "11.1.2.11"}
        calls = []

        def rpc(state, slot, method, *args):
            calls.append((slot, method))
            if method == "getpeerinfo":
                peer = slots["reg0" if slot == "node0" else "node0"]
                return json.dumps([{"addr": peer + ":19799", "version": 1}])
            if method == "getnetworkinfo":
                return json.dumps({"localaddresses": [{"address": slots[slot], "port": gen.P2P}]})
            return {"getblockcount": "1", "-named": "{}", "getnewaddress": "miner",
                    "generatetoaddress": '["b2","b3","b4"]', "getbestblockhash": "b4",
                    "getblockchaininfo": '{"chain":"devnet-test"}'}[method]

        with tempfile.TemporaryDirectory() as tmp, patch.object(lifecycle, "rpc", rpc), \
                patch.object(lifecycle, "start_harness", side_effect=lambda *_: calls.append(("harness", "start"))):
            lifecycle.check_peering({"run_dir": tmp, "slots": slots, "harness": True})
            peers = json.loads((Path(tmp) / "peering.json").read_text())["peers_after_harness"]
            self.assertEqual(peers, [slots["reg0"]])
        start = calls.index(("harness", "start"))
        for slot in slots:
            self.assertIn((slot, "getpeerinfo"), calls[:start])
            self.assertIn((slot, "getnetworkinfo"), calls[:start])
        self.assertLess(calls.index(("node0", "getpeerinfo"), start),
                        calls.index(("node0", "generatetoaddress")))

    def test_whole_chain_coverage(self):
        b.check_coverage([1, 2, 3, 4, 5], 5)
        for heights in ([], [4, 5], [1, 3, 4, 5], [1, 2, 2, 3, 4, 5], [1, 2, 3, 4]):
            with self.subTest(heights=heights), self.assertRaisesRegex(ValueError, "every height"):
                b.check_coverage(heights, 5)

    def test_funding(self):
        self.assertEqual(b.COLLATERAL, 68000)
        self.assertEqual(b.funding_height(7, gen.CONSENSUS), 176)
        with self.assertRaisesRegex(ValueError, "window"):
            b.funding_height(7, {**gen.CONSENSUS, "highsubsidyblocks": 175})

    def test_ready_before_sporks(self):
        registrations = {s: s + "-hash" for s in b.SLOTS}
        rows = {s: {"state": "READY", "proTxHash": h} for s, h in registrations.items()}
        b.ready_guard(rows, registrations)
        driver = object.__new__(b.Bootstrap)
        driver.registrations = registrations
        driver.rpc = Mock()
        for bad in ({}, {**rows, "reg3": {"state": "WAITING_FOR_PROTX"}},
                    {**rows, "evo2": rows["evo1"]}):
            driver.records = {"steps": {"ready": bad}}
            with self.assertRaisesRegex(ValueError, "READY"):
                driver.sporks()
            driver.rpc.assert_not_called()

    def test_consensus_state_and_confs(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp).resolve()
            values = {"highsubsidyblocks": 154, "highsubsidyfactor": 900, "minimumdifficultyblocks": 1500}
            state = gen.state_for(gen.network("11.2.3.0/24"), {"node0": "dashd", "evo0": "dashd"},
                                  "x", dest, "a" * 32, values, {"git_head": "a" * 40,
                                  "mixnet-watch": "b" * 64, "mixnet-judge": "c" * 64})
            gen.write_json(dest / "state.json", state)
            self.assertEqual(lifecycle.state_at(dest)["consensus"], values)
            for slot in state["slots"]:
                conf = gen.config(state, slot, "u", "p").splitlines()
                for k, v in values.items():
                    self.assertEqual(conf.count(f"{k}={v}"), 1)
            service = gen.compose_for(state)["services"]["harness"]
            self.assertEqual(service["networks"]["devnet"]["ipv4_address"], "11.2.3.2")
            self.assertEqual(service["labels"], gen.labels(state))
            self.assertIn("profiles", service)
            for value in (0, -1, True, 2**31):
                with self.assertRaisesRegex(ValueError, "settings"):
                    gen.state_for(gen.network("11.2.3.0/24"), {}, "x", dest, "a" * 32,
                                  {**values, "highsubsidyfactor": value})

    def test_mnhf_serialization(self):
        self.assertEqual(b.hash256(b"\x04mnhf" + (28).to_bytes(8, "little")),
                         "a0eee872d7d3170dd20d5c5e8380c92b3aa887da5f63d8033289fafa35a90691")
        quorum = bytes(range(32))
        tx = b.mnhf_tx(14, quorum.hex(), bytes(96))
        self.assertEqual(tx[:13].hex(), "0300070000000000000082010e")
        self.assertEqual(tx[13:45], quorum[::-1])
        self.assertEqual(tx[45:], bytes(96))


class CommitmentTests(unittest.TestCase):
    def setUp(self):
        self.audit = b.Commitments()
        self.evos = {str(n) for n in range(12)}

    def add(self, kind, base, index=0, valid=True, evos=None):
        count = b.SIZES[kind]
        qhash = f"{base:064x}"
        qc = {"llmqType": kind, "quorumHash": qhash, "validMembersCount": count, "signersCount": count}
        info = {"quorumHash": qhash, "type": b.TYPES[kind], "height": base, "quorumIndex": index,
                "members": [{"proTxHash": str(n), "valid": valid or n != 0} for n in range(count)]}
        self.audit.check(base + 12, qc, lambda *_: info, self.evos if evos is None else evos)

    def populate(self):
        for base in (240, 288):
            for k in b.SIZES:
                self.add(k, base)
            self.add(105, base + 1, 1)

    def test_complete_cycles(self):
        self.populate()
        self.audit.complete()
        for k in b.SIZES:
            rows = self.audit.finals[k]
            self.audit.finals[k] = rows[:1]
            with self.assertRaisesRegex(ValueError, b.TYPES[k]):
                self.audit.complete()
            self.audit.finals[k] = rows
        self.audit.finals[105].pop()
        with self.assertRaisesRegex(ValueError, "rotating cycles"):
            self.audit.complete()

    def test_member_and_platform_assertions(self):
        for k in b.SIZES:
            with self.subTest(k=k), self.assertRaisesRegex(ValueError, "invalid"):
                self.add(k, 240, valid=False)
        with self.assertRaisesRegex(ValueError, "non-EvoNode"):
            self.add(107, 240, evos={"0"})
        self.add(107, 240)
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.add(107, 240)

    def test_five_types(self):
        for k in b.TYPES.keys() - b.SIZES.keys():
            qc = {"llmqType": k, "quorumHash": "0" * 64, "validMembersCount": 0, "signersCount": 0}
            qc.update(quorumPublicKey="0" * 96, quorumVvecHash="0" * 64, quorumSig="0" * 192,
                      membersSig="0" * 192)
            self.audit.check(15, qc, None, set())
            with self.assertRaisesRegex(ValueError, "non-null"):
                self.audit.check(16, {**qc, "quorumVvecHash": "f" * 64}, None, set())
            with self.subTest(k=k), self.assertRaisesRegex(ValueError, f"non-null {b.TYPES[k]}"):
                self.audit.check(16, {**qc, "signersCount": 1}, None, set())
        self.assertEqual(list(self.audit.report()["five_types"].values()), [{"non_null": 0, "null": 1}] * 5)


if __name__ == "__main__":
    unittest.main()
