#!/usr/bin/env python3
"""The two regtest dashds the probes talk to.

By default one per pinned Core release (v23.1.8 and v24.0.0-rc.3). With
DDE_PAIR=v24 both run v24.0.0-rc.3, the only parity reference (DEC-82): the
node under test, and a peer for the probes that need a second node.

Each node runs from an unpacked release tarball with its own datadir under
DDE_PROBE_DIR (default /work/scratch/dde-probe). Never point this at the shared
oracles under /work/chains or the benchmark nodes under /work/bench.
"""
import json
import os
import pathlib
import subprocess
import sys
import time

ROOT = pathlib.Path(os.environ.get("DDE_PROBE_DIR", "/work/scratch/dde-probe"))

# DDE_PORT_BASE moves both nodes' ports, so two probe runs can share the box.
PORT_BASE = int(os.environ.get("DDE_PORT_BASE", "29800"))

V24_RELEASE = os.environ.get("DDE_DASHD_V24", str(ROOT / "dashcore-24.0.0-rc.3"))

# name -> (unpacked release directory, P2P port, RPC port, datadir name)
if os.environ.get("DDE_PAIR", "releases") == "v24":
    NODES = {
        "v24.0.0-rc.3": (V24_RELEASE, PORT_BASE + 21, PORT_BASE + 22, "d24"),
        "v24.0.0-rc.3-peer": (V24_RELEASE, PORT_BASE + 31, PORT_BASE + 32, "d24peer"),
    }
else:
    NODES = {
        "v23.1.8": (os.environ.get("DDE_DASHD_V23", str(ROOT / "dashcore-23.1.8")), PORT_BASE + 11, PORT_BASE + 12,
                    "d23"),
        "v24.0.0-rc.3": (V24_RELEASE, PORT_BASE + 21, PORT_BASE + 22, "d24"),
    }

# The nodes whose behaviour is the evidence: in the v24 pair, not the peer.
UNDER_TEST = [n for n in NODES if not n.endswith("-peer")]

CONF = """regtest=1
server=1
listen=1
listenonion=0
discover=0
dnsseed=0
fixedseeds=0
debug=net
logips=1
capturemessages=1
printtoconsole=0
rpcuser=dde
rpcpassword=dde
[regtest]
port={port}
rpcport={rpc}
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
bind=127.0.0.1
"""


def datadir(node):
    return ROOT / NODES[node][3]


def p2p_port(node):
    return NODES[node][1]


def log_path(node):
    return datadir(node) / "regtest" / "debug.log"


def cli(node, *args):
    release = NODES[node][0]
    cmd = [os.path.join(release, "bin", "dash-cli"), "-datadir=%s" % datadir(node)] + list(args)
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode:
        raise RuntimeError(r.stderr.strip())
    out = r.stdout.strip()
    return json.loads(out) if out.startswith(("{", "[")) else out


def start(*extra):
    """Starts both nodes; `extra` dashd arguments (e.g. -blocksonly=1) apply to both."""
    for node, (release, port, rpc, _) in NODES.items():
        d = datadir(node)
        d.mkdir(parents=True, exist_ok=True)
        (d / "dash.conf").write_text(CONF.format(port=port, rpc=rpc))
        subprocess.Popen([os.path.join(release, "bin", "dashd"), "-datadir=%s" % d, *extra],
                         stdout=open(d / "stdout.log", "a"), stderr=subprocess.STDOUT, start_new_session=True)
    for node in NODES:
        # A cold start on slow disks can take minutes (LevelDB opens).
        for _ in range(900):
            try:
                info = cli(node, "getnetworkinfo")
                break
            except RuntimeError:
                time.sleep(1)
        print(node, info["subversion"])


def stop():
    """Stops both nodes and waits until their RPC is gone."""
    for node in NODES:
        try:
            cli(node, "stop")
        except RuntimeError as e:
            print(node, e)
    for node in NODES:
        for _ in range(60):
            try:
                cli(node, "getblockcount")
                time.sleep(1)
            except RuntimeError:
                break
        pid = datadir(node) / "regtest" / "dashd.pid"
        for _ in range(60):
            if not pid.exists():
                break
            time.sleep(1)


def restart(*extra):
    stop()
    start(*extra)


if __name__ == "__main__":
    {"start": start, "stop": stop}[sys.argv[1]](*sys.argv[2:])
