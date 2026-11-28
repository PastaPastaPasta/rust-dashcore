#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "$0")" && pwd)
exec python3 -I -B - "$here" "$@" <<'PY'
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

here = Path(sys.argv.pop(1))
p = argparse.ArgumentParser(description="Exercise termination and recovery using the full devnet plan")
p.add_argument("--subnet", default=os.environ.get("DEVNET_SUBNET"))
p.add_argument("--plan", type=Path, required=True)
p.add_argument("--dashd", type=Path, required=True)
p.add_argument("--harness-tree", type=Path, required=True)
p.add_argument("--run-dir", type=Path, required=True)
args = p.parse_args()
slots = json.loads(args.plan.read_text())["slots"]
if len(slots) != 33 or any(s["runs"] != "dashd" for s in slots):
    p.error("expected the full 33-slot dashd devnet plan")
if not args.subnet:
    p.error("subnet is required")
args.run_dir.mkdir(parents=True, mode=0o700, exist_ok=True)
if any(args.run_dir.iterdir()):
    p.error("run directory must be empty")
up = [here / "up.sh", "--subnet", args.subnet, "--plan", args.plan, "--dashd", args.dashd,
      "--harness-tree", args.harness_tree]
down = [here / "down.sh", "--force", "--subnet", args.subnet]


def ensure(condition, reason):
    if not condition:
        raise RuntimeError(reason)


def case(name, sig, expected):
    directory = args.run_dir / name
    payload = [] if sig else ["--", "sh", "-c", 'test -s "$DEVNET_RUN_DIR/peering.json" && '
                                                 'docker exec "$COMPOSE_PROJECT_NAME-harness" test -x /usr/local/bin/mixnet-watch && exit 3']
    with (args.run_dir / f"{name}.log").open("w") as log:
        proc = subprocess.Popen([*up, "--run-dir", directory, *payload], stdout=log, stderr=log)
        try:
            if sig:
                deadline = time.monotonic() + 300
                phase = directory / "phase"
                while True:
                    ensure(proc.poll() is None and time.monotonic() < deadline, f"{name}: missed startup")
                    if sig == signal.SIGKILL:
                        if phase.is_file() and phase.read_text().strip() == "compose-start":
                            state = json.loads((directory / "state.json").read_text())
                            present = subprocess.check_output(["docker", "ps", "-aq", "--filter",
                                                   f"label=org.dash.o12b.run={state['run_id']}"])
                            if present.strip():
                                break
                    elif phase.is_file() and phase.read_text().strip() == "rpc-wait":
                        break
                    time.sleep(0.05)
                proc.send_signal(sig)
            status = proc.wait(timeout=600)
            ensure(status == expected, f"{name}: expected {expected}, got {status}")
            if sig == signal.SIGKILL:
                state = json.loads((directory / "state.json").read_text())
                remaining = subprocess.check_output(["docker", "ps", "-aq", "--filter",
                                            f"label=org.dash.o12b.run={state['run_id']}"])
                ensure(bool(remaining.strip()), "SIGKILL left no containers")
                retry = subprocess.run([*up, "--run-dir", args.run_dir / "retry"],
                                       capture_output=True, text=True, timeout=300)
                ensure(retry.returncode == 2 and "down.sh --force --subnet" in retry.stderr,
                       f"second up.sh did not refuse leftovers: {retry.stderr}")
                subprocess.run(down, check=True, stdout=log, stderr=log)
                subprocess.run(down, check=True, stdout=log, stderr=log)
            subprocess.run([sys.executable, "-I", "-B", here / "lifecycle.py", "verify",
                            "--run-dir", directory], check=True)
            ensure(not (directory / "rpc.cookie").exists(), f"{name}: harness cookie remains")
            ensure(not list(directory.glob("nodes/*/dash.conf")), f"{name}: credentials remain")
        finally:
            if proc.poll() is None:
                proc.terminate()
                proc.wait(timeout=180)
            if (directory / "state.json").is_file():
                subprocess.run([here / "down.sh", "--force", "--run-dir", directory],
                               check=True, stdout=log, stderr=log)
    print(f"{name}: PASS (up.sh status {status})", flush=True)


case("term", signal.SIGTERM, 143)
case("int", signal.SIGINT, 130)
case("payload", None, 3)
case("kill", signal.SIGKILL, -signal.SIGKILL)
PY
