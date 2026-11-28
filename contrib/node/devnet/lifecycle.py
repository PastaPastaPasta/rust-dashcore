"""Host operations shared by devnet startup, teardown and its signal tests."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gen


def docker(*args, **kwargs):
    return subprocess.run(["docker", *args], check=True, text=True, **kwargs)


def objects(kind, *labels):
    filters = [arg for label in labels for arg in ("--filter", "label=" + label)]
    args = ["docker", kind, "ls", "-q", *filters]
    if kind == "container":
        args.append("-a")
    return gen.output(*args).split()


KINDS = ("container", "network", "volume", "image")


def preflight(subnet):
    net = gen.network(subnet)
    gen.check_addresses(net)
    headroom()
    gen.output("docker", "info", "--format", "{{.ServerVersion}}")
    for kind in KINDS:
        if objects(kind, gen.RUN, f"{gen.SUBNET}={net}"):
            raise ValueError(f"leftover {kind} on {net}; run down.sh --force --subnet {net}")


def headroom():
    if shutil.disk_usage("/work").free < 150 * 1024**3 or os.getloadavg()[1] >= 300:
        raise ValueError("need 150 GiB free on /work and load5 below 300")


def state_at(directory):
    data = json.loads((directory / "state.json").read_text())
    slots = gen.parse_slots({"v": 1, "context": "devnet", "slots": [
        {"slot": slot, "runs": kind} for slot, kind in data["runs"].items()]})
    expected = gen.state_for(gen.network(data["subnet"]), slots, data["devnet_name"],
                             directory.resolve(), data["run_id"], data.get("consensus"), data.get("harness"))
    if "consensus" not in data and "harness" not in data:
        del expected["consensus"], expected["harness"]
    if data != expected:
        raise ValueError(f"inconsistent state in {directory}")
    return data


def build(state):
    label_args = [arg for k, v in gen.labels(state).items() for arg in ("--label", f"{k}={v}")]
    for kind, tag in state["images"].items():
        subprocess.run(["setpriv", "--pdeathsig", "TERM", "--", "docker", "build", "--network=none",
                        "--pull=false", *label_args, "-t", tag, str(Path(state["run_dir"]) / "build" / kind)],
                       check=True)


def rpc(state, slot, *args):
    cli = "rdash-cli" if state["runs"][slot] == "rust" else "dash-cli"
    return subprocess.run(["docker", "exec", f"{state['project']}-{slot}", cli,
                           "-datadir=/data", "-conf=/etc/dash.conf",
                           f"-rpcconnect={state['slots'][slot]}", f"-rpcport={gen.RPC}", *args],
                          text=True, capture_output=True, check=True, timeout=12).stdout.strip()


def eventually(probe, description, timeout=120):
    until = time.monotonic() + timeout
    detail = ""
    while time.monotonic() < until:
        try:
            result = probe()
            if result and time.monotonic() <= until:
                return result
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
            detail = str(getattr(exc, "stderr", "") or exc)
        time.sleep(0.3)
    raise ValueError(f"timeout waiting for {description}: {detail.strip()}")


def check_peering(state, restarted=False):
    slots = state["slots"]
    directory = Path(state["run_dir"])
    with ThreadPoolExecutor(max_workers=8) as pool:
        def all_slots(fn):
            return dict(zip(slots, pool.map(fn, slots)))

        (directory / "phase").write_text("rpc-wait\n")
        eventually(lambda: all_slots(lambda slot: rpc(state, slot, "getblockcount")), "slot RPC")
        (directory / "phase").write_text("peering\n")

        def peers(slot):
            return sorted(p["addr"].rsplit(":", 1)[0]
                          for p in json.loads(rpc(state, slot, "getpeerinfo"))
                          if p["version"] > 0)

        def topology():
            connections = all_slots(peers)
            if connections["node0"] != sorted(ip for slot, ip in slots.items() if slot != "node0"):
                return None
            if all(slots["node0"] in connections[s] for s in slots if s != "node0"):
                return connections

        connections = eventually(topology, "node0 peer connections")

        def advertises(slot):
            addresses = json.loads(rpc(state, slot, "getnetworkinfo"))["localaddresses"]
            return any(a["address"] == slots[slot] and a["port"] == gen.P2P for a in addresses)

        local = all_slots(advertises)
        if not all(local.values()):
            raise ValueError(f"missing externalip: {[s for s in slots if not local[s]]}")
        if not restarted:
            if state.get("harness"):
                rpc(state, "node0", "-named", "createwallet", "wallet_name=seed",
                    "descriptors=false", "load_on_startup=true")
            else:
                rpc(state, "node0", "createwallet", "seed")
        after_start = {}
        if state.get("harness") and restarted:
            address, = json.loads((directory / "nodes.json").read_text())["coinbase"]
        else:
            address = rpc(state, "node0", "getnewaddress")
        if state.get("harness") and not restarted:
            start_harness(state, address)
            observed = peers("node0")
            if observed != connections["node0"]:
                raise ValueError("node0 P2P peers changed after starting the harness")
            after_start["peers_after_harness"] = observed
        mined = json.loads(rpc(state, "node0", "generatetoaddress", "3", address))
        if len(mined) != 3:
            raise ValueError(f"mined {len(mined)} blocks")

        def tips():
            current = all_slots(lambda slot: rpc(state, slot, "getbestblockhash"))
            if set(current.values()) == {mined[-1]}:
                return current

        agreed = eventually(tips, "common mined tip")
        chain = json.loads(rpc(state, "node0", "getblockchaininfo"))["chain"]
        gen.write_json(directory / ("peering-restarted.json" if restarted else "peering.json"), {"peers": connections, "externalip": local,
                       "mined": mined, "tips": agreed, "common_tip": mined[-1], "chain": chain, **after_start})
        (directory / "phase").write_text("ready\n")


def start_harness(state, address):
    directory = Path(state["run_dir"])
    nodes = directory / "nodes.json"
    config = json.loads(nodes.read_text())
    config["coinbase"] = {address: "node0"}
    gen.write_json(nodes, config)
    observations = directory / "observations"
    os.mkfifo(observations / "watch.events", mode=0o600)
    headroom()
    docker("compose", "-p", state["project"], "-f", str(directory / "compose.yaml"),
           "up", "-d", "--pull", "never", "harness")

    def first_watermark():
        if (observations / "watch.exit").exists():
            raise ValueError("watcher exited before mining; see observations/watch.log")
        log = observations / "obs.jsonl"
        if log.exists():
            with log.open("rb") as f:
                for line in f:
                    if line.endswith(b"\n"):
                        record = json.loads(line)
                        if record["kind"] == "Watermark":
                            if record["height"] != 1:
                                raise ValueError("watcher missed devnet height 1")
                            return True
        return False

    eventually(first_watermark, "height-1 watcher watermark")


def subnet_runs(subnet):
    found = {}
    for kind in KINDS:
        ids = objects(kind, gen.RUN, f"{gen.SUBNET}={subnet}")
        if ids:
            for obj in json.loads(gen.output("docker", kind, "inspect", *ids)):
                labels = obj.get("Config", obj)["Labels"]
                directory = Path(labels[gen.DIRECTORY])
                state = state_at(directory)
                if gen.labels(state) != {k: labels[k] for k in gen.labels(state)} or state["subnet"] != subnet:
                    raise ValueError("Docker labels and run state disagree")
                found[state["run_id"]] = state
    return list(found.values())


def route_for(subnet):
    return gen.output("ip", "-4", "route", "show", "exact", subnet)


def inventory(state):
    return {kind: objects(kind, f"{gen.RUN}={state['run_id']}") for kind in KINDS}


def verify(state):
    remaining = {k: v for k, v in inventory(state).items() if v}
    route = route_for(state["subnet"])
    if remaining or route:
        raise ValueError(f"teardown incomplete: objects={remaining}, route={route!r}")


def copy_logs(state, volumes):
    project = state["project"]
    where = f"/data/devnet-{state['devnet_name']}/debug.log"
    tags = [arg for k, v in gen.labels(state).items() for arg in ("--label", f"{k}={v}")]
    containers = set(gen.output("docker", "container", "ls", "-a", "--format", "{{.Names}}",
                                "--filter", f"label={gen.RUN}={state['run_id']}").split())

    def copy(slot):
        name = f"{project}-{slot}"
        volume = name + "-data"
        if volume not in volumes:
            return
        dest = Path(state["run_dir"]) / "nodes" / slot / "debug.log"
        if name in containers:
            result = subprocess.run(["docker", "cp", f"{name}:{where}", str(dest)],
                                    capture_output=True, text=True)
            if result.returncode == 0:
                return
            if "Could not find the file" in result.stderr:
                return
        image = state["images"][state["runs"][slot]]
        headroom()
        with dest.open("wb") as out:
            docker("run", "--rm", "--pull=never", "--network=none", *tags, "--entrypoint", "sh",
                   "-v", f"{volume}:/data:ro", image, "-c",
                   'if [ -f "$1" ]; then cat "$1"; fi', "sh", where, stdout=out)

    with ThreadPoolExecutor(max_workers=6) as pool:
        list(pool.map(copy, state["slots"]))


def down(state, force):
    current = inventory(state)
    subnet = state["subnet"]
    if not force:
        print(json.dumps({"directory": state["run_dir"], "objects": current, "route": route_for(subnet)}))
        return
    directory = Path(state["run_dir"])
    log_error = None
    try:
        if current["volume"]:
            copy_logs(state, set(current["volume"]))
    except (ValueError, OSError, subprocess.SubprocessError) as exc:
        log_error = exc
    if any(current[k] for k in ("container", "network", "volume")):
        subprocess.run(["docker", "compose", "-p", state["project"], "-f", str(directory / "compose.yaml"),
                        "down", "-v", "--remove-orphans", "-t", "10"], check=False)
    for kind in KINDS:
        ids = objects(kind, f"{gen.RUN}={state['run_id']}")
        if ids:
            force_args = ["-f"] if kind == "container" else []
            subprocess.run(["docker", kind, "rm", *force_args, *ids], check=False)
    for line in route_for(subnet).splitlines():
        fields = line.split()
        if fields[0] == subnet and "dev" in fields and fields[fields.index("dev") + 1] == state["bridge"]:
            subprocess.run(["sudo", "-n", "ip", "route", "del", subnet, "dev", state["bridge"]], check=True)
    for slot in state["slots"]:
        (directory / "nodes" / slot / "dash.conf").unlink(missing_ok=True)
    for name in ("rpc.cookie", "run-seed", "observations/watch.events"):
        (directory / name).unlink(missing_ok=True)
    verify(state)
    if log_error:
        raise ValueError(f"could not archive debug logs: {log_error}")


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="action", required=True)
    commands.add_parser("preflight").add_argument("--subnet", required=True)
    for cmd in ("build", "peering", "verify"):
        commands.add_parser(cmd).add_argument("--run-dir", type=Path, required=True)
    stop = commands.add_parser("down")
    stop.add_argument("--force", action="store_true")
    selector = stop.add_mutually_exclusive_group(required=True)
    selector.add_argument("--run-dir", type=Path)
    selector.add_argument("--subnet")
    args = parser.parse_args()
    if args.action == "preflight":
        preflight(args.subnet)
    elif args.action == "down":
        if args.run_dir:
            down(state_at(args.run_dir), args.force)
        else:
            subnet = str(gen.network(args.subnet))
            runs = subnet_runs(subnet)
            for state in runs:
                down(state, args.force)
            if not runs:
                route = route_for(subnet)
                print(f"no labelled objects on {subnet}; route={route!r}")
                if args.force and route:
                    raise ValueError("route has no owning run")
    else:
        {"build": build, "peering": check_peering, "verify": verify}[args.action](state_at(args.run_dir))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as exc:
        sys.exit(gen.refusal(exc))
