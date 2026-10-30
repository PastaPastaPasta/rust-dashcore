#!/usr/bin/env python3
"""BL-175's Core capture: a regtest dashd v24 mines blocks that signal unknown version bits, and
this records what Core does with them.

    BL175_DASHD=<dashd> python3 -I contrib/node/bl175-alert-captures/capture.py <work-dir> <out-dir>

One dashd, a fresh datadir, `-mocktime=1700000000` (the differential's clock), no listening. It
mines heights 1-250 with version 0x20c00000 (bits 22 and 23), restarts, then mines 251-431
with 0x20400000 (bit 22), through `-blockversion`. Regtest's warning window is 144 blocks with a
threshold of 108 (`chainparams.cpp`), so in the period 144-287 bit 22 is signalled 144 times and
bit 23 107 times: bit 22 is LOCKED_IN from tip 287 and ACTIVE at tip 431, and bit 23 never leaves
STARTED. Tip 431 is the last block before V20 (regtest's DIP0003 height, 432).

`-alertnotify` is `sh '<dir>/record.sh' '<dir>/alert.txt' %s %s; true`. `record.sh` appends, for
each run, its parent shell's argv (`/proc/$PPID/cmdline`: glibc 2.38's `system` runs
`sh -c -- <command>`, older ones `sh -c <command>`; `; true` keeps that shell from exec'ing the
script) and the arguments the
script received. In the
fixture the datadir's path reads `<dir>`.

Written to `<out-dir>`, line-oriented so the Rust test reads it with no parser:

- `blocks.txt`: the raw blocks at heights 1-431, one hex line each, in order.
- `record.sh`: the script above.
- `capture.txt`, one record a line:
  - `# ...`: provenance: dashd's `-version` line and sha256, the host's glibc, the arguments of
    both runs (datadir `<dir>`), the mock time, the date;
  - `template <-alertnotify>`;
  - `tip <height> <hash><warning>` for every `UpdateTip` line of debug.log (432: genesis's load,
    then each block), `<warning>` being the line's ` warning='...'` suffix, or nothing;
  - `warnings <rpc> <text>`: the `warnings` field of `getblockchaininfo`, `getnetworkinfo` and
    `getmininginfo` at tip 431;
  - `alert <line>`: each line `record.sh` wrote;
  - `run_command_error <line>`: debug.log's `runCommand error` lines (none).
"""

import base64
import datetime
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

DASHD = os.environ.get("BL175_DASHD")
MOCKTIME = 1_700_000_000
RPC_PORT = int(os.environ.get("BL175_RPC_PORT", "31975"))
# `-blockversion` reads a decimal integer (`GetIntArg`).
PHASES = [(250, str(0x20C00000)), (431, str(0x20400000))]

RECORD_SH = """out=$1
shift
{
  tr '\\0' '\\n' < /proc/$PPID/cmdline | sed 's/^/sh_argv=/'
  printf 'argc=%s\\n' "$#"
  for a in "$@"; do printf 'arg=%s\\n' "$a"; done
} >> "$out"
"""


def rpc(method, *params):
    req = urllib.request.Request(
        f"http://127.0.0.1:{RPC_PORT}/",
        data=json.dumps({"jsonrpc": "1.0", "id": 0, "method": method, "params": list(params)}).encode(),
        headers={"Authorization": "Basic " + base64.b64encode(b"bl175:bl175").decode()})
    try:
        with urllib.request.urlopen(req, timeout=600) as r:
            reply = json.load(r)
    except urllib.error.HTTPError as e:
        reply = json.load(e)
    if reply.get("error"):
        raise RuntimeError(f"{method}{params}: {reply['error']}")
    return reply["result"]


def args(datadir, version, alertnotify):
    return [f"-datadir={datadir}", "-regtest", "-server", "-listen=0", "-listenonion=0",
            "-discover=0", "-dnsseed=0", "-fixedseeds=0", "-upnp=0", "-natpmp=0", "-disablewallet",
            "-printtoconsole=0", f"-rpcport={RPC_PORT}", "-rpcbind=127.0.0.1",
            "-rpcallowip=127.0.0.1", "-rpcuser=bl175", "-rpcpassword=bl175",
            f"-mocktime={MOCKTIME}", f"-blockversion={version}", f"-alertnotify={alertnotify}"]


def run(datadir, version, alertnotify, stderr):
    proc = subprocess.Popen([DASHD, *args(datadir, version, alertnotify)],
                            stdout=subprocess.DEVNULL, stderr=stderr)
    end = time.time() + 900
    while True:
        # Only this dashd answers: one that exited (a port taken by another) fails here.
        if proc.poll() is not None or time.time() > end:
            proc.kill()
            raise RuntimeError(f"dashd did not start; see {datadir}/regtest/debug.log")
        try:
            rpc("getblockcount")
            return proc
        except Exception:
            time.sleep(0.5)


def stop(proc):
    rpc("stop")
    proc.wait(timeout=600)


def version_line(work):
    # A private datadir: `-version` rewrites the datadir's settings.json.
    probe = work / "version-probe"
    probe.mkdir(mode=0o700)
    out = subprocess.run([DASHD, f"-datadir={probe}", "-version"], capture_output=True, text=True,
                         check=True).stdout
    shutil.rmtree(probe)
    return out.splitlines()[0]


def main():
    if not DASHD or len(sys.argv) != 3:
        sys.exit(__doc__)
    work, out = Path(sys.argv[1]).resolve(), Path(sys.argv[2])
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True, mode=0o700)
    out.mkdir(parents=True, exist_ok=True)
    datadir = work / "node"
    datadir.mkdir(mode=0o700)
    (datadir / "record.sh").write_text(RECORD_SH)
    alert = datadir / "alert.txt"
    template = "sh '<dir>/record.sh' '<dir>/alert.txt' %s %s; true"
    alertnotify = template.replace("<dir>", str(datadir))
    version = version_line(work)
    stderr = open(work / "stderr.txt", "wb")
    runs = []
    for tip, blockversion in PHASES:
        runs.append([a.replace(str(datadir), "<dir>") for a in args(datadir, blockversion, alertnotify)])
        proc = run(datadir, blockversion, alertnotify, stderr)
        try:
            while (height := rpc("getblockcount")) < tip:
                rpc("generatetodescriptor", min(100, tip - height), "raw(51)")
            if tip == PHASES[-1][0]:
                end = time.time() + 60
                while not alert.exists():
                    if time.time() > end:
                        raise RuntimeError("no alertnotify run")
                    time.sleep(0.2)
                # The detached command has written its lines by then.
                time.sleep(2)
                warnings = {m: rpc(m)["warnings"]
                            for m in ("getblockchaininfo", "getnetworkinfo", "getmininginfo")}
                blocks = [rpc("getblock", rpc("getblockhash", h), 0) for h in range(1, tip + 1)]
        except BaseException:
            proc.kill()
            raise
        stop(proc)
    stderr.close()

    log = (datadir / "regtest" / "debug.log").read_text()
    update_tip = []
    for m in re.finditer(r"UpdateTip: new best=([0-9a-f]{64}) height=(\d+) .*?( warning='[^']*')?$",
                         log, re.M):
        update_tip.append([int(m.group(2)), m.group(1), m.group(3) or ""])
    assert [u[0] for u in update_tip] == list(range(432)), "one UpdateTip a height, genesis's load first"
    sha256 = hashlib.sha256(Path(DASHD).read_bytes()).hexdigest()
    lines = [f"# {version}, dashd sha256 {sha256}, captured {datetime.date.today().isoformat()}",
             f"# by contrib/node/bl175-alert-captures/capture.py, -mocktime={MOCKTIME}, "
             f"host {' '.join(platform.libc_ver())}"]
    lines += [f"# run {i + 1}: dashd {' '.join(r)}" for i, r in enumerate(runs)]
    lines.append(f"template {template}")
    lines += [f"tip {h} {hash_}{warning}" for h, hash_, warning in update_tip]
    lines += [f"warnings {m} {w}" for m, w in sorted(warnings.items())]
    lines += [f"alert {l}" for l in alert.read_text().replace(str(datadir), "<dir>").splitlines()]
    lines += [f"run_command_error {l}" for l in log.splitlines() if "runCommand error" in l]
    (out / "blocks.txt").write_text("".join(b + "\n" for b in blocks))
    (out / "record.sh").write_text(RECORD_SH)
    (out / "capture.txt").write_text("".join(l + "\n" for l in lines))

if __name__ == "__main__":
    main()
