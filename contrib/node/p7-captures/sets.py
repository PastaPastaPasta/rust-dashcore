#!/usr/bin/env python3
"""Record Core v24.0.0 llmq/net_quorum.cpp:408-497, :766-834 from running nodes."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import time

CORE_COMMIT = "938fba502824b9d850b7c710601456dc3d71603e"
SPORK = "SPORK_21_QUORUM_ALL_CONNECTED"
MARKERS = (
    "EnsureQuorumConnections --",
    "NetQuorum::CheckQuorumConnections --",
    "NetQuorum::GetQuorumsToDelete --",
)


def rpc(cli, node, *params):
    result = subprocess.run(
        [str(cli), f"-datadir={node['datadir']}", f"-rpcport={node['rpc_port']}",
         "-regtest", *map(str, params)],
        capture_output=True, text=True, timeout=30, check=False,
    )
    if result.returncode:
        raise RuntimeError(f"RPC {params[0]} failed (rc {result.returncode})")
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return result.stdout.strip()


def connection_lines(text):
    lines = []
    continued = False
    for line in text.splitlines():
        marker = next((marker for marker in MARKERS if marker in line), None)
        if marker:
            lines.append(line[line.index(marker):])
            continued = marker == MARKERS[0] and "adding masternodes" in line
        elif continued and re.fullmatch(r"  [0-9a-f]{64} \([^\r\n]*\)", line):
            lines.append(line)
        else:
            continued = False
    return sorted(lines)


def capture(args):
    commit = subprocess.check_output(
        ["git", "-C", str(args.core), "rev-parse", "HEAD"], text=True,
    ).strip()
    if commit != CORE_COMMIT:
        raise RuntimeError("Core tree is not the v24.0.0 provenance commit")
    nodes = json.loads(args.nodes.read_text())
    if not nodes:
        raise RuntimeError("at least one running node is required")
    for node in nodes:
        if rpc(args.cli, node, "getnetworkinfo")["version"] != 240000:
            raise RuntimeError("every node must report Core version 240000")
    logs = [Path(node["datadir"]) / "regtest/debug.log" for node in nodes]
    offsets = [path.stat().st_size for path in logs]
    controller = nodes[0]
    # Core: rpc/node.cpp:156-245; llmq/options.cpp:26-40
    if rpc(args.cli, controller, "sporkupdate", SPORK, args.spork_value) != "success":
        raise RuntimeError("spork 21 update was rejected")
    deadline = time.monotonic() + args.timeout
    while True:
        values = [rpc(args.cli, node, "spork", "show")[SPORK] for node in nodes]
        if all(value == args.spork_value for value in values):
            break
        if time.monotonic() >= deadline:
            raise RuntimeError("spork 21 did not reach the requested value on every node")
        time.sleep(0.2)
    if args.mine_block:
        address = rpc(args.cli, controller, "getnewaddress")
        rpc(args.cli, controller, "generatetoaddress", 1, address)
    deadline = time.monotonic() + args.timeout
    while True:
        tips = [rpc(args.cli, node, "getbestblockhash") for node in nodes]
        if len(set(tips)) == 1:
            break
        if time.monotonic() >= deadline:
            raise RuntimeError("nodes did not converge on a common tip")
        time.sleep(0.2)
    observations = []
    for node, path, offset in zip(nodes, logs, offsets):
        tip = rpc(args.cli, node, "getblockchaininfo")
        base = tip["blocks"] - tip["blocks"] % args.dkg_interval
        heights = range(base, min(tip["blocks"] + 1, base + args.active_quorums)) if args.rotation else [base]
        ancestors = {str(h): rpc(args.cli, node, "getblockhash", h) for h in heights}
        quorums = []
        for spec in args.quorum:
            llmq_type, quorum_hash = spec.split(":", 1)
            # Core: rpc/quorums.cpp:249-307; includeSkShare=false
            info = rpc(args.cli, node, "quorum", "info", int(llmq_type), quorum_hash, "false")
            missing_keys = [key for key in ("height", "type", "quorumHash", "minedBlock", "members") if key not in info]
            if missing_keys:
                raise RuntimeError(f"quorum info {spec} lacks {', '.join(missing_keys)}")
            quorums.append({
                key: info[key] for key in ("height", "type", "quorumHash", "quorumIndex", "minedBlock", "members") if key in info
            })
        if path.stat().st_size < offset:
            raise RuntimeError("debug.log was truncated during capture")
        with path.open("rb") as log:
            log.seek(offset)
            lines = connection_lines(log.read().decode("utf-8", errors="strict"))
        observations.append({
            "tip": {"height": tip["blocks"], "hash": tip["bestblockhash"]},
            "spork21": rpc(args.cli, node, "spork", "show")[SPORK],
            "ancestors": ancestors,
            "quorums": quorums,
            "lines": lines,
        })
    if any(node["spork21"] != args.spork_value for node in observations):
        raise RuntimeError("spork 21 changed during capture")
    missing = []
    if not all(any("EnsureQuorumConnections --" in line for line in node["lines"]) for node in observations):
        missing.append("EnsureQuorumConnections lines on every node")
    if not all(any("NetQuorum::GetQuorumsToDelete --" in line for line in node["lines"]) for node in observations):
        missing.append("GetQuorumsToDelete lines on every node")
    if not args.quorum:
        missing.append("ordered quorum info members (--quorum TYPE:HASH)")
    with args.dashd.open("rb") as binary:
        binary_sha256 = hashlib.file_digest(binary, "sha256").hexdigest()
    output = {
        "core_commit": commit,
        "core_sources": ["src/llmq/net_quorum.cpp:408-497,766-834", "src/llmq/utils.cpp:754-853"],
        "binary_sha256": binary_sha256,
        "spork21": args.spork_value,
        "rotation": args.rotation,
        "dkg_interval": args.dkg_interval,
        "active_quorums": args.active_quorums,
        "observations": observations,
        "missing": missing,
        "replay_ready": False,
    }
    args.output.write_text(json.dumps(output, indent=2) + "\n")
    return 2 if missing else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--dashd", type=Path, required=True)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--nodes", type=Path, required=True)
    parser.add_argument("--spork-value", type=int, choices=[0, 4070908800], required=True)
    parser.add_argument("--dkg-interval", type=int, required=True)
    parser.add_argument("--active-quorums", type=int, required=True)
    parser.add_argument("--rotation", action="store_true")
    parser.add_argument("--quorum", action="append", default=[], metavar="TYPE:HASH")
    parser.add_argument("--mine-block", action="store_true")
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.dkg_interval <= 0 or args.active_quorums <= 0 or args.timeout <= 0:
        parser.error("interval, active quorum count and timeout must be positive")
    try:
        return capture(args)
    except (OSError, ValueError, KeyError, TypeError, RuntimeError, subprocess.SubprocessError) as error:
        detail = str(error) if isinstance(error, RuntimeError) else type(error).__name__
        print(f"capture failed: {detail}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
