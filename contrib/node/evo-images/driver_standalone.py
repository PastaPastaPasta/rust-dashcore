#!/usr/bin/env python3
"""Standalone run of the BL-875 chain driver on stock regtest (no recorder around it).

Run through driver-standalone.sh, which puts Core v24.0.0's test/functional on PYTHONPATH. node0 is the controller
and only miner; nodes 1..12 are masternodes 0..11 (0..7 regular, 8..11 evo; all basic-BLS, register_fund) registered
in block 432. Every block is mined by ChainDriver.mine_block(). From the first height an LLMQ_TEST quorum can sign,
ChainLocks are on and the masternodes' own V24 signal is held for block 650. Cuts are captured at --cuts, by default
at heights congruent mod 24 to the real cuts (S5 1240 -> 664, S5a 990 -> 678, S4 1290 -> 690, S3 1300 -> 700), and
their keys.json-style "quorums" are written to --out.

Keys: `--testkit-keys <bin> --key-seed-file <0600 file>` (both required). The operator keys come from
`testkit-keys bls-derive`, which is handed the file's path (`--seed-file`), never the seed itself, so the seed is on no
command line. Quorum sks come from `testkit-keys quorum-sk`, which reads the members' shares on stdin.
"""
import json
import os
import subprocess

from test_framework.script import hash160
from test_framework.test_framework import DashTestFramework, MasternodeInfo
from test_framework.util import assert_equal, p2p_port

from driver import (LLMQS, LLMQ_TEST, LLMQ_TEST_DIP0024, ChainDriver, TestkitQuorumKeys, check_private_file,
                    withhold_unknown_args, write_private_json)

CHAIN_ARGS = ["-vbparams=testdummy:-2:9223372036854775807", "-sporkaddr=yj949n1UH6fDhw6HtVE5VMj2iSTaSWBMcW"]
N_MNS = 12
N_EVO = 4
REG_HEIGHT = 432
ENABLE_HEIGHT = 441
EHF_BIT = 13
EHF_HEIGHT = 650


class OperatorKeys:
    """Operator keys 0..count-1 from `testkit-keys bls-derive --seed-file`. Errors never repeat the tool's output,
    which holds sks."""

    def __init__(self, binary, seed_file, timeout=60):
        check_private_file(seed_file)
        self.binary, self.seed_file, self.timeout = binary, seed_file, timeout

    def keys(self, count):
        cmd = [self.binary, "bls-derive", "--seed-file", self.seed_file, "--index", "0", "--count", str(count)]
        try:
            res = subprocess.run(cmd, capture_output=True, text=True, check=False, timeout=self.timeout)
        except subprocess.TimeoutExpired:
            raise RuntimeError(f"testkit-keys bls-derive timed out after {self.timeout:.0f}s") from None
        if res.returncode != 0:
            raise RuntimeError(f"testkit-keys bls-derive exit {res.returncode} (output withheld)")
        try:
            rows = [json.loads(line) for line in res.stdout.splitlines()]
            if [(r["index"], r["scheme"]) for r in rows] != [(i, "basic") for i in range(count)]:
                raise ValueError
            return [(r["sk"], r["pk"]) for r in rows]
        except (ValueError, KeyError, TypeError):
            raise RuntimeError("testkit-keys bls-derive printed unexpected rows (output withheld)") from None


class DriverStandalone(DashTestFramework):
    def add_options(self, parser):
        # No prefix matching, so an old `--key-seed <hex>` is not read as --key-seed-file. It is then an unknown
        # argument, and argparse would echo it in its error; withhold_unknown_args drops the values from that error.
        parser.allow_abbrev = False
        withhold_unknown_args(parser)
        self.add_wallet_options(parser)
        parser.add_argument("--until", type=int, default=700)
        parser.add_argument("--cuts", default="S5:664,S5a:678,S4:690,S3:700")
        parser.add_argument("--out", default=None)
        parser.add_argument("--testkit-keys", required=True)
        parser.add_argument("--key-seed-file", required=True)

    def set_test_params(self):
        self.set_dash_test_params(N_MNS + 1, N_MNS, extra_args=[list(CHAIN_ARGS) for _ in range(N_MNS + 1)],
                                  evo_count=N_EVO)

    def append_dip3_config(self, datadir):
        # stock regtest: no dip3params / testactivationheight (DEC-1666 (a))
        pass

    def setup_network(self):
        self.add_nodes(1, extra_args=[self.extra_args[0]])
        self.start_node(0)
        self.init_wallet(node=0)

    def run_test(self):
        if self.options.usecli:
            raise ValueError("--usecli would put member shares and sks on dash-cli command lines")
        node0 = self.nodes[0]
        out = self.options.out or os.path.join(self.options.tmpdir, "out")
        os.makedirs(out, mode=0o700, exist_ok=True)
        os.chmod(out, 0o700)
        cuts = {int(h): name for name, h in (c.split(":") for c in self.options.cuts.split(","))}
        tool_timeout = 60 * self.options.timeout_factor
        opkeys = OperatorKeys(self.options.testkit_keys, self.options.key_seed_file, timeout=tool_timeout)
        keys = TestkitQuorumKeys(self.options.testkit_keys, core_node=node0, timeout=tool_timeout)
        driver = ChainDriver(self, self.mninfo, keys)

        self.log.info("Mining to 424")
        while node0.getblockcount() < 424:
            driver.mine_block()

        mns = []
        for i, (sk, pk) in enumerate(opkeys.keys(N_MNS)):
            mn = MasternodeInfo(evo=i >= N_MNS - N_EVO, legacy=False)
            mn.generate_addresses(node0)
            # before DIP3 enforcement (500) the voting key must be the owner key (bad-protx-key-not-same)
            mn.votingAddr = mn.ownerAddr
            mn.keyOperator, mn.pubKeyOperator = sk, pk
            node0.sendtoaddress(mn.fundsAddr, mn.get_collateral_value() + 1)
            mns.append(mn)
        while node0.getblockcount() < REG_HEIGHT - 1:
            driver.mine_block()

        self.log.info("Registering %d masternodes (%d evo) at tip %d", N_MNS, N_EVO, node0.getblockcount())
        for i, mn in enumerate(mns):
            port = p2p_port(i + 1)
            kw = {}
            if mn.evo:
                kw = dict(platform_node_id=hash160(b"bl875-standalone-%d" % i).hex(),
                          addrs_platform_p2p=port + 101, addrs_platform_https=port + 102)
            protx = mn.register_fund(node0, submit=True, addrs_core_p2p=[f"127.0.0.1:{port}"], **kw)
            mn.set_params(proTxHash=protx, operator_reward=0, collateral_txid=protx, nodePort=port)
            self.mninfo.append(mn)
        bh = driver.mine_block()
        block = node0.getblock(bh, 2)
        assert_equal(block["height"], REG_HEIGHT)
        assert_equal(sorted(t["txid"] for t in block["tx"] if t.get("type") == 1),
                     sorted(mn.proTxHash for mn in mns))

        self.start_masternodes()
        while node0.getblockcount() < ENABLE_HEIGHT - 1:
            driver.mine_block()
        assert_equal(len(node0.masternodelist("status")), N_MNS)

        driver.hold_ehf(EHF_BIT, EHF_HEIGHT)
        driver.enable(ENABLE_HEIGHT)
        cl_on = None
        captures = {}
        while node0.getblockcount() < self.options.until:
            driver.mine_block()
            h = node0.getblockcount()
            if cl_on is None and driver.signing_ready(LLMQ_TEST):
                driver.enable_chainlocks()
                cl_on = h
            if h in cuts:
                name = cuts[h]
                quorums = driver.capture(name, h)
                captures[name] = {"T": h, "quorums": quorums,
                                  "checks": [{"id": c, "ok": ok, "detail": d} for c, ok, d in driver.checks[name]]}
                write_private_json(os.path.join(out, f"{name}-quorums.json"), captures[name])

        records = driver.all_records(node0.getblockcount())
        commitments = [{k: r[k] for k in ("height", "type", "index", "base_height", "quorum_hash")}
                       for r in records if not r["null"]]
        cycles = {}
        for r in commitments:
            if r["type"] == LLMQ_TEST_DIP0024:
                cycles.setdefault(r["base_height"] - r["index"], {})[r["index"]] = r["height"]
        summary = {
            "tip": node0.getblockcount(),
            "rotation_ready_base": driver.rotation_ready_base,
            "first_base": driver.first_base,
            "chainlocks_on_at": cl_on,
            "best_chainlock": node0.getbestchainlock(),
            "ehf": driver.ehf_state(),
            "mnhf_blocks": driver.mnhf,
            "recorder_sporks": driver.recorder_sporks(),
            "dip0024_cycles": {b: cycles[b] for b in sorted(cycles)},
            "commitments_by_type": {LLMQS[t].name: [(r["base_height"], r["height"]) for r in commitments
                                                    if r["type"] == t] for t in LLMQS},
            "v24": node0.getdeploymentinfo()["deployments"]["v24"],
            "captures": {k: {"T": v["T"], "n_quorums": len(v["quorums"]),
                             "checks": [(c["id"], c["ok"]) for c in v["checks"]]} for k, v in captures.items()},
        }
        write_private_json(os.path.join(out, "summary.json"), summary)
        self.log.info("summary: %s", json.dumps(summary, default=str))
        assert driver.rotation_ready_base is not None
        assert cl_on is not None
        if self.options.until >= EHF_HEIGHT:
            assert_equal(sorted(driver.mnhf), [EHF_HEIGHT])
        for name, cap in captures.items():
            assert all(c["ok"] for c in cap["checks"]), name


if __name__ == "__main__":
    DriverStandalone().main()
