"""Run with python3 -I -B contrib/node/devnet/test_gen.py."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen
import lifecycle


class NetworkTests(unittest.TestCase):
    def test_classes(self):
        for prefix, reason in [
                ("10.9.9", "RFC1918"), ("172.31.255", "RFC1918"), ("192.168.0", "RFC1918"),
                ("198.19.0", "RFC2544"), ("169.254.1", "RFC3927"), ("100.64.0", "RFC6598"),
                ("192.0.2", "RFC5737"), ("198.51.100", "RFC5737"), ("203.0.113", "RFC5737"),
                ("0.0.3", "local"), ("127.9.1", "local"), ("255.255.255", "invalid"),
                ("224.1.2", "global"), ("240.1.2", "global")]:
            with self.subTest(prefix=prefix), self.assertRaisesRegex(ValueError, reason):
                gen.check_addresses(gen.network(prefix + ".0/24"))
        gen.check_addresses(gen.network("11.253.247.0/24"))

    def test_network_address(self):
        for spec in ("11.1.2.9/24", "11.0.0.0/8", "::/24", "fe80::/24", "fc00::/24",
                     "2001:10::/28", "2001:20::/28", "2001:db8::/32", None):
            with self.subTest(spec=spec), self.assertRaises(ValueError):
                gen.network(spec)

    def test_route_and_network_overlap(self):
        net = gen.network("11.2.3.0/24")
        default = [{"dst": "default", "dev": "eth0", "gateway": "192.168.1.1"}]
        for dest in ("11.2.3.5", "11.2.3.128/25", "11.2.0.0/16"):
            with self.subTest(dest=dest), self.assertRaisesRegex(ValueError, "host route"):
                gen.check_routes(net, [*default, {"type": "local", "table": "local", "dst": dest}], [])
        gen.check_routes(net, default, ["fd00::/64", "11.2.4.0/24"])
        with self.assertRaisesRegex(ValueError, "Docker network"):
            gen.check_routes(net, default, ["11.0.0.0/8"])

    def test_tcp_and_udp_peers(self):
        net = gen.network("11.2.3.0/24")
        for proto in ("tcp", "udp"):
            with self.subTest(proto=proto), self.assertRaisesRegex(ValueError, "current peer"):
                gen.check_peers(net, f"{proto} ESTAB 0 0 10.1.2.3:33111 11.2.3.19:443")
        gen.check_peers(net, "tcp ESTAB 0 0 11.2.3.5:443 12.0.0.1:51\nudp UNCONN 0 0 0.0.0.0:99 *:*")

    def test_resolver(self):
        net = gen.network("11.2.3.0/24")
        expected = {"test": ["11.2.4.4", "12.0.0.1"]}
        self.assertEqual(gen.resolve_hosts(net, ["test", "test"], expected.get), expected)
        with self.assertRaisesRegex(ValueError, "needed host"):
            gen.resolve_hosts(net, ["test"], lambda _: ["14.0.0.1", "11.2.3.1"])
        with self.assertRaisesRegex(ValueError, "cannot resolve"):
            gen.resolve_hosts(net, ["test"], lambda _: [])

        def fail(_):
            raise socket.gaierror("no IPv4")

        with self.assertRaisesRegex(ValueError, "cannot resolve"):
            gen.resolve_hosts(net, ["test"], fail)


class GeneratorTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.base = Path(tmp.name)
        self.files = {"dashd": b"core", "dash-cli": b"cli", "rdashd": b"rust", "custom-cli": b"rcli"}
        for name, content in self.files.items():
            (self.base / name).write_bytes(content)
        self.pin = self.base / "dashd-v24.0.0.sha256"
        self.pin.write_text("".join(f"{hashlib.sha256(self.files[k]).hexdigest()}  bin/{k}\n"
                                    for k in ("dashd", "dash-cli")))
        self.names = ["node0", *(f"reg{i}" for i in range(20)), *(f"evo{i}" for i in range(12))]
        self.plan = {"v": 1, "context": "devnet",
                     "slots": [{"slot": s, "runs": "dashd"} for s in self.names]}
        self.dashd = self.base / "dashd"
        self.rdashd = self.base / "rdashd"

    def test_plan(self):
        self.assertEqual(list(gen.parse_slots(self.plan)), self.names)
        for overrides in ({"v": 2}, {"v": True}, {"context": "regtest"}, {"slots": []},
                          {"slots": [self.plan["slots"][0]] * 246},
                          {"slots": [{"slot": "../test", "runs": "dashd"}]},
                          {"slots": [self.plan["slots"][0]] * 2}):
            with self.subTest(overrides=list(overrides)), self.assertRaises(ValueError):
                gen.parse_slots({**self.plan, **overrides})

    def test_needed_hosts_include_proxies(self):
        config = self.base / ".codex"
        config.mkdir()
        (config / "config.toml").write_text('[model_providers.example]\nbase_url="https://model.test:888/v1"\n')
        args = argparse.Namespace(needed_hosts=gen.HERE / "needed-hosts.txt", needed_host=["extra.test"])
        with patch.object(Path, "home", return_value=self.base), \
                patch.dict(os.environ, {"ANTHROPIC_BASE_URL": "https://api.test/v1"}):
            names = gen.needed_hosts(args)
        for name in ("model.test", "api.test", "github.com", "extra.test"):
            self.assertIn(name, names)

    def test_binary_routing(self):
        core = {"node0": "dashd"}
        rust = {**core, "reg0": "rust"}
        for slots, rdashd in ((core, self.rdashd), (rust, None)):
            with self.assertRaisesRegex(ValueError, "--rdashd"):
                gen.binaries(slots, self.dashd, rdashd, self.pin)
        with self.assertRaisesRegex(ValueError, "same sha256"):
            gen.binaries(rust, self.dashd, self.dashd, self.pin)
        for binary in ("dashd", "dash-cli"):
            with self.subTest(binary=binary), self.assertRaisesRegex(ValueError, binary):
                (self.base / binary).write_bytes(b"tampered")
                gen.binaries(core, self.dashd, None, self.pin)
            (self.base / binary).write_bytes(self.files[binary])

    def test_generated_run(self):
        here = gen.HERE
        for daemon in ("dashd", "rdashd"):
            dockerfile = (here / f"Dockerfile.{daemon}").read_text()
            cli = "rdash-cli" if daemon == "rdashd" else "dash-cli"
            self.assertRegex(dockerfile, r"^FROM ubuntu@sha256:[a-f0-9]{64}\n")
            self.assertIn(f"{daemon} {cli} /usr/local/bin/", dockerfile)
            self.assertIn(f'ENTRYPOINT ["{daemon}"', dockerfile)
            self.assertIn("USER ubuntu", dockerfile)
            (self.base / f"Dockerfile.{daemon}").write_text(dockerfile)
        self.plan["slots"][1]["runs"] = "rust"
        plan_path = self.base / "plan"
        plan_path.write_text(json.dumps(self.plan))
        run_dir = self.base / "run"
        args = argparse.Namespace(plan=plan_path, dashd=self.dashd, rdashd=self.rdashd,
                                  subnet="11.2.3.0/24", devnet_name="test", run_dir=run_dir)
        resolved = {"github.com": ["12.0.0.1"]}
        with patch.object(gen, "HERE", self.base), patch.object(gen, "live_checks", return_value=resolved), \
                patch.dict(os.environ, {"RDASH_CLI": str(self.base / "custom-cli")}):
            gen.generate(args)
            with self.assertRaisesRegex(ValueError, "not empty"):
                gen.generate(args)
        self.assertEqual(run_dir.stat().st_mode & 0o777, 0o700)
        self.assertEqual(json.loads((run_dir / "needed-hosts.resolved.json").read_text()), resolved)
        self.assertEqual((run_dir / "build/rust/rdash-cli").read_bytes(), b"rcli")
        state = json.loads((run_dir / "state.json").read_text())
        compose = json.loads((run_dir / "compose.yaml").read_text())
        network = compose["networks"]["devnet"]
        self.assertTrue(network["internal"])
        self.assertEqual(network["ipam"]["config"], [{"subnet": args.subnet, "gateway": "11.2.3.1"}])
        self.assertEqual(network["labels"][gen.RUN], state["run_id"])
        self.assertEqual(state["project"], "o12b-devnet-" + state["run_id"])
        with patch.object(lifecycle.subprocess, "run") as command:
            lifecycle.build(state)
            self.assertEqual(command.call_count, 2)
            for call in command.call_args_list:
                build = call.args[0]
                self.assertIn(f"{gen.RUN}={state['run_id']}", build)
                self.assertIn(build[build.index("-t") + 1], state["images"].values())
            for slot, cli in (("reg0", "rdash-cli"), ("node0", "dash-cli")):
                lifecycle.rpc(state, slot, "getblockcount")
                self.assertEqual(command.call_args.args[0][3], cli)
        for index, slot in enumerate(self.names, 10):
            with self.subTest(slot=slot):
                ip = f"11.2.3.{index}"
                self.assertEqual(state["slots"][slot], ip)
                conf = (run_dir / "nodes" / slot / "dash.conf").read_text().splitlines()
                self.assertEqual(conf[:2], ["devnet=test", "[devnet]"])
                for line in (f"bind={ip}:19799", f"externalip={ip}:19799", f"rpcbind={ip}",
                             f"rpcallowip={args.subnet}", "rpcport=19798", "port=19799",
                             "upnp=0", "natpmp=0", "dnsseed=0", "fixedseeds=0"):
                    self.assertIn(line, conf)
                self.assertEqual("addnode=11.2.3.10:19799" in conf, slot != "node0")
                self.assertEqual("disablewallet=1" in conf, slot != "node0")
                service = compose["services"][slot]
                self.assertEqual(service["networks"]["devnet"]["ipv4_address"], ip)
                self.assertEqual(network["labels"], service["labels"])
                self.assertEqual(network["labels"], compose["volumes"][slot]["labels"])
                self.assertIn(f"{slot}:/data", service["volumes"])
                kind = "rust" if slot == "reg0" else "dashd"
                self.assertEqual(service["image"], state["images"][kind])
                self.assertIn(state["run_id"], service["image"])
                self.assertNotIn("restart", service)


if __name__ == "__main__":
    unittest.main()
