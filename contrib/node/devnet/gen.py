"""Generate the bridge, node configs and build contexts for a devnet RolePlan."""

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import socket
import subprocess
import sys
import tomllib
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parent
RUN = "org.dash.o12b.run"
SUBNET = "org.dash.o12b.subnet"
DIRECTORY = "org.dash.o12b.directory"
P2P = 19799
RPC = 19798
CONSENSUS = {"highsubsidyblocks": 200, "highsubsidyfactor": 1000, "minimumdifficultyblocks": 4096}
# Dash Core v24.0.2 src/llmq/params.h, CDevNetParams.
QUORUMS = [
    (1, "llmq_50_60", 50, 40, 30, 24, 10, 18, False, 24, False),
    (2, "llmq_400_60", 400, 300, 240, 288, 20, 28, False, 4, False),
    (3, "llmq_400_85", 400, 350, 340, 576, 20, 48, False, 4, False),
    (4, "llmq_100_67", 100, 80, 67, 24, 10, 18, False, 24, False),
    (5, "llmq_60_75", 60, 50, 45, 288, 42, 50, True, 32, False),
    (101, "llmq_devnet", 12, 7, 6, 24, 10, 18, False, 4, False),
    (105, "llmq_devnet_dip0024", 8, 6, 4, 48, 12, 20, True, 2, False),
    (107, "llmq_devnet_platform", 12, 9, 8, 24, 10, 18, False, 4, True),
]
PARAM_FIELDS = ("type", "name", "size", "min_size", "threshold", "dkg_interval",
                "mining_window_start", "mining_window_end", "rotation", "signing_active_quorum_count",
                "evo_only")

# Dash Core v24.0.2 src/netaddress.cpp, CNetAddr::IsValid/IsRoutable (IPv4).
CORE_CLASSES = {
    "RFC1918": ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"),
    "RFC2544": ("198.18.0.0/15",),
    "RFC3927": ("169.254.0.0/16",),
    "RFC6598": ("100.64.0.0/10",),
    "RFC5737": ("192.0.2.0/24", "198.51.100.0/24", "203.0.113.0/24"),
    "local": ("0.0.0.0/8", "127.0.0.0/8"),
    "invalid": ("255.255.255.255/32",),
}


def network(spec):
    try:
        net = ipaddress.IPv4Network(spec)
        if net.prefixlen == 24:
            return net
    except (TypeError, ValueError):
        pass
    raise ValueError("subnet must be an IPv4 /24 network address")


def check_addresses(net):
    for kind, prefixes in CORE_CLASSES.items():
        for prefix in prefixes:
            if net.overlaps(ipaddress.IPv4Network(prefix)):
                raise ValueError(f"{net} overlaps Core's {kind} address class")
    for addr in net.hosts():
        if not addr.is_global or addr.is_reserved or addr.is_multicast:
            raise ValueError(f"{addr} is not global unicast")


def check_routes(net, routes, docker_subnets):
    for route in routes:
        dest = route.get("dst", "default")
        if dest not in ("default", "0.0.0.0/0") and net.overlaps(ipaddress.IPv4Network(dest)):
            raise ValueError(f"{net} overlaps host route {dest}")
    for dest in docker_subnets:
        other = ipaddress.ip_network(dest)
        if other.version == 4 and net.overlaps(other):
            raise ValueError(f"{net} overlaps Docker network {dest}")


def check_peers(net, lines):
    for line in lines.splitlines():
        peer, port = line.split()[-1].rsplit(":", 1)
        if peer != "*" and ipaddress.IPv4Address(peer) in net:
            raise ValueError(f"current peer {peer}:{port} is inside {net}")


def resolve4(name):
    return {entry[4][0] for entry in socket.getaddrinfo(name, None, socket.AF_INET)}


def resolve_hosts(net, names, resolver=resolve4):
    result = {}
    for name in sorted(set(names)):
        try:
            result[name] = sorted(set(resolver(name)))
        except OSError as exc:
            raise ValueError(f"cannot resolve needed IPv4 host {name}: {exc}") from exc
        if not result[name]:
            raise ValueError(f"cannot resolve needed IPv4 host {name}")
        for address in result[name]:
            if ipaddress.IPv4Address(address) in net:
                raise ValueError(f"needed host {name} resolves to {address} inside {net}")
    return result


def needed_hosts(args):
    names = args.needed_hosts.read_text().split() + args.needed_host
    urls = [os.environ.get("ANTHROPIC_BASE_URL")]
    config = Path.home() / ".codex/config.toml"
    if config.exists():
        with config.open("rb") as f:
            settings = tomllib.load(f)
        urls.append(settings.get("base_url"))
        urls.extend(p.get("base_url") for p in settings.get("model_providers", {}).values())
    for url in filter(None, urls):
        host = urlsplit(url).hostname
        if not host:
            raise ValueError("configured proxy URL has no hostname")
        names.append(host)
    return names


def output(*args):
    return subprocess.check_output(args, text=True, stderr=subprocess.PIPE).strip()


def live_checks(net, args):
    resolved = resolve_hosts(net, needed_hosts(args))
    routes = json.loads(output("ip", "-j", "-4", "route", "show", "table", "all"))
    ids = output("docker", "network", "ls", "-q").split()
    nets = json.loads(output("docker", "network", "inspect", *ids)) if ids else []
    check_routes(net, routes, (c["Subnet"] for n in nets for c in n["IPAM"]["Config"] or []
                               if "Subnet" in c))
    check_peers(net, output("ss", "-H", "-tun4"))
    return resolved


def parse_slots(plan):
    if not isinstance(plan, dict) or type(plan.get("v")) is not int or plan["v"] != 1:
        raise ValueError("expected RolePlan v1")
    if plan.get("context") != "devnet":
        raise ValueError("expected devnet context")
    records = plan.get("slots")
    if not isinstance(records, list) or not 1 <= len(records) <= 245:
        raise ValueError("slots must fit in .10 through .254")
    slots = {}
    for record in records:
        slot, runs = record["slot"], record["runs"]
        if not re.fullmatch(r"node0|(?:reg|evo)(?:0|[1-9][0-9]*)", slot) or slot in slots:
            raise ValueError(f"invalid or repeated slot {slot!r}")
        if runs not in ("dashd", "rust"):
            raise ValueError(f"unknown implementation for {slot}")
        slots[slot] = runs
    if next(iter(slots)) != "node0":
        raise ValueError("first slot must be node0")
    return slots


def sha(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def check_core(dashd, pin):
    pins = {}
    for line in pin.read_text().splitlines():
        digest, name = line.split()
        pins[name] = digest
    cli = dashd.with_name("dash-cli")
    for name, path in (("dashd", dashd), ("dash-cli", cli)):
        if sha(path) != pins[f"bin/{name}"]:
            raise ValueError(f"{name} sha256 differs from the release pin")
    return pins["bin/dashd"]


def binaries(slots, dashd, rdashd, pin):
    core_hash = check_core(dashd, pin)
    result = {"dashd": {"dashd": dashd, "dash-cli": dashd.with_name("dash-cli")}}
    if ("rust" in slots.values()) != (rdashd is not None):
        raise ValueError("--rdashd is required exactly when a slot runs Rust")
    if rdashd:
        if sha(rdashd) == core_hash:
            raise ValueError("rdashd and dashd have the same sha256")
        rcli = Path(os.environ.get("RDASH_CLI", rdashd.with_name("rdash-cli")))
        if not rcli.is_file():
            raise ValueError("cannot find rdash-cli (RDASH_CLI)")
        result["rust"] = {"rdashd": rdashd, "rdash-cli": rcli}
    return result


def check_harness_glibc(program):
    versions = re.findall(r"\bGLIBC_(\d+(?:\.\d+)+)\b", output("readelf", "-WV", program))
    required = max((tuple(map(int, v.split("."))) for v in versions), default=())
    if required > (2, 39):
        version = ".".join(map(str, required))
        raise ValueError(f"{program.name} requires glibc {version}; pinned Noble provides 2.39")


def labels(state):
    return {RUN: state["run_id"], SUBNET: state["subnet"], DIRECTORY: state["run_dir"]}


def state_for(net, slots, name, run_dir, run_id, consensus=None, harness=None):
    consensus = CONSENSUS if consensus is None else consensus
    if consensus.keys() != CONSENSUS.keys() or any(type(v) != int or not 1 <= v <= 2147483647
                                                    for v in consensus.values()):
        raise ValueError("invalid devnet subsidy or difficulty settings")
    kinds = set(slots.values())
    if harness is not None:
        if set(harness) != {"git_head", "mixnet-watch", "mixnet-judge"} or not all(
                re.fullmatch(r"[a-f0-9]{40}" if k == "git_head" else r"[a-f0-9]{64}", v)
                for k, v in harness.items()) or kinds != {"dashd"}:
            raise ValueError("invalid Core-only rehearsal provenance")
        kinds.add("harness")
    if not re.fullmatch(r"[a-zA-Z0-9_-]+", name):
        raise ValueError("invalid devnet name")
    if not re.fullmatch(r"[a-f0-9]{32}", run_id):
        raise ValueError("invalid run id")
    project = "o12b-devnet-" + run_id
    return {"run_id": run_id, "project": project, "network": project + "-net",
            "bridge": "o12b-" + run_id[:10], "subnet": str(net), "devnet_name": name,
            "run_dir": str(run_dir), "runs": slots, "consensus": consensus, "harness": harness,
            "slots": {slot: str(net[i]) for i, slot in enumerate(slots, 10)},
            "images": {k: f"{project}-{k}:run" for k in sorted(kinds)}}


def config(state, slot, user, password):
    ip = state["slots"][slot]
    lines = [f"devnet={state['devnet_name']}", "[devnet]", *[f"{k}={v}" for k, v in state["consensus"].items()], "listen=1", f"bind={ip}:{P2P}",
             f"externalip={ip}:{P2P}", f"port={P2P}", f"rpcbind={ip}", f"rpcconnect={ip}",
             f"rpcallowip={state['subnet']}", f"rpcport={RPC}", f"rpcuser={user}",
             f"rpcpassword={password}", "dnsseed=0", "fixedseeds=0", "upnp=0", "natpmp=0",
             "listenonion=0", "dbcache=16", "par=1", "noconnect=1"]
    if state["harness"]:
        lines.extend(("txindex=1", "blockfilterindex=1", "peerblockfilters=1", "peerbloomfilters=1",
                      "deprecatedrpc=softforks", "debug=llmq", "debug=llmq-dkg", "debug=mnsync"))
    if slot != "node0":
        lines.extend(("disablewallet=1", f"addnode={state['slots']['node0']}:{P2P}"))
    return "\n".join(lines) + "\n"


def compose_for(state):
    tags = labels(state)
    net = network(state["subnet"])
    bridge = {"name": state["network"], "driver": "bridge", "internal": True, "labels": tags,
              "driver_opts": {"com.docker.network.bridge.name": state["bridge"]},
              "ipam": {"config": [{"subnet": str(net), "gateway": str(net[1])}]}}
    services, volumes = {}, {}
    for slot, ip in state["slots"].items():
        volumes[slot] = {"name": f"{state['project']}-{slot}-data", "labels": tags}
        services[slot] = {"image": state["images"][state["runs"][slot]], "pull_policy": "never",
                          "labels": tags, "container_name": f"{state['project']}-{slot}",
                          "networks": {"devnet": {"ipv4_address": ip}},
                          "volumes": [f"{slot}:/data", {"type": "bind", "read_only": True,
                              "source": f"./nodes/{slot}/dash.conf", "target": "/etc/dash.conf"}]}
        if slot != "node0":
            services[slot]["depends_on"] = ["node0"]
    if state["harness"]:
        services["harness"] = {
            "image": state["images"]["harness"], "pull_policy": "never", "profiles": ["rehearsal"],
            "labels": tags, "container_name": state["project"] + "-harness",
            "user": f"{os.getuid()}:{os.getgid()}",
            "networks": {"devnet": {"ipv4_address": str(net[2])}},
            "volumes": ["./nodes.json:/nodes.json:ro", "./rpc.cookie:/rpc.cookie:ro",
                        "./plan.json:/plan.json:ro", "./observations:/observations"],
        }
    return {"name": state["project"], "services": services, "networks": {"devnet": bridge},
            "volumes": volumes}


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n")


def generate(args):
    net = network(args.subnet)
    check_addresses(net)
    plan = json.loads(args.plan.read_text())
    slots = parse_slots(plan)
    bins = binaries(slots, args.dashd, args.rdashd, HERE / "dashd-v24.0.0.sha256")
    resolved = live_checks(net, args)
    dest = args.run_dir.resolve()
    harness = None
    if getattr(args, "harness_tree", None):
        tree = args.harness_tree.resolve()
        bins["harness"] = {n: tree / "target/debug" / n for n in ("mixnet-watch", "mixnet-judge")}
        for binary in bins["harness"].values():
            check_harness_glibc(binary)
        harness = {n: sha(binary) for n, binary in bins["harness"].items()}
        harness["git_head"] = output("git", "-C", tree, "rev-parse", "HEAD")
        if plan.get("preset") != "devnet" or plan.get("caps") != ["mn", "qdata"]:
            raise ValueError("expected the devnet mn,qdata rehearsal plan")
    consensus = {k: getattr(args, k, v) for k, v in CONSENSUS.items()}
    state = state_for(net, slots, args.devnet_name, dest,
                      os.environ.get("O12B_RUN_ID", secrets.token_hex(16)), consensus, harness)
    dest.mkdir(mode=0o700, parents=True, exist_ok=True)
    if any(dest.iterdir()):
        raise ValueError(f"run directory is not empty: {dest}")
    dest.chmod(0o700)
    with (dest / "state.json").open("x") as f:
        json.dump(state, f, indent=2)
        f.write("\n")
    write_json(dest / "needed-hosts.resolved.json", resolved)
    write_json(dest / "compose.yaml", compose_for(state))
    for kind, programs in bins.items():
        ctx = dest / "build" / kind
        ctx.mkdir(parents=True)
        daemon = next(iter(programs))
        shutil.copyfile(HERE / ("Dockerfile.harness" if kind == "harness" else f"Dockerfile.{daemon}"), ctx / "Dockerfile")
        for target, src in programs.items():
            shutil.copyfile(src, ctx / target)
        if kind == "harness" and any(sha(ctx / n) != harness[n] for n in programs):
            raise ValueError("harness binary changed during staging")
        if kind == "dashd":
            check_core(ctx / daemon, HERE / "dashd-v24.0.0.sha256")
    user, password = secrets.token_hex(12), secrets.token_hex(32)
    if harness:
        write_json(dest / "plan.json", plan)
        (dest / "observations").mkdir(mode=0o700)
        (dest / "rpc.cookie").write_text(f"{user}:{password}")
        (dest / "rpc.cookie").chmod(0o600)
        write_json(dest / "nodes.json", {
            "v": 1, "run_id": state["run_id"], "run": {
                "run_kind": "devnet-rehearsal", "context": "devnet", "preset": plan["preset"],
                "caps": plan["caps"], "host": {"nproc": len(os.sched_getaffinity(0)),
                    "mem": os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES"),
                    "disk": shutil.disk_usage(dest).free}, "clock": {"source": "monotonic", "offset_ms": 0},
                "binaries": {"dashd": {"sha256": sha(args.dashd), "version": "24.0.0"}, "rdashd": None}},
            "nodes": [{"slot": s, "runs": slots[s], "rpc": f"{ip}:{RPC}", "cookie": "/rpc.cookie",
                       "chainlocks": True, "chaintips": True} for s, ip in state["slots"].items()],
            "llmq_params": [dict(zip(PARAM_FIELDS, q, strict=True)) for q in QUORUMS], "coinbase": {}})
    for slot in slots:
        directory = dest / "nodes" / slot
        directory.mkdir(parents=True)
        conf = directory / "dash.conf"
        conf.write_text(config(state, slot, user, password))
        conf.chmod(0o444)


class ArgumentParser(argparse.ArgumentParser):
    def error(self, message):
        self.exit(2, f"refused: {message}\n")


def main():
    p = ArgumentParser()
    p.add_argument("--subnet", default=os.environ.get("DEVNET_SUBNET"))
    p.add_argument("--plan", type=Path, required=True)
    p.add_argument("--dashd", type=Path, required=True)
    p.add_argument("--rdashd", type=Path)
    p.add_argument("--harness-tree", type=Path)
    for flag, key in zip(("--high-subsidy-blocks", "--high-subsidy-factor", "--minimum-difficulty-blocks"),
                         CONSENSUS, strict=True):
        p.add_argument(flag, dest=key, type=int, default=CONSENSUS[key])
    p.add_argument("--devnet-name", default="o12b")
    p.add_argument("--run-dir", type=Path, required=True)
    p.add_argument("--needed-hosts", type=Path, default=HERE / "needed-hosts.txt")
    p.add_argument("--needed-host", action="append", default=[])
    try:
        generate(p.parse_args())
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        sys.exit(refusal(exc))


def refusal(exc):
    print("refused:", " ".join(str(exc).splitlines()), file=sys.stderr)
    return 2


if __name__ == "__main__":
    main()
