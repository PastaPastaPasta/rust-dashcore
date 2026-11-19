#!/usr/bin/env python3
"""IM1a R2-M1: a `fee_estimates.dat` that a real Core v24.0.0-rc.3 loaded and wrote back with a NaN
decay in every horizon.

Core's `TxConfirmStats::Read` throws only for `decay <= 0 || decay >= 1` (policy/fees.cpp), and both
comparisons are false for NaN, so the file loads and is written again at shutdown. The input is the
captured `populated/fee_estimates.dat` with the three decay words replaced by a quiet NaN; the
output is the file Core wrote at `stop`, copied byte for byte.

    python3 -I im1a-nan-decay.py <dashd> <dash-cli> <populated/fee_estimates.dat> <out.dat>

Run it with TMPDIR on a private directory (the node's datadir is created there).
"""
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import tempfile

# policy/fees.h: SHORT_DECAY, MED_DECAY, LONG_DECAY. File order: medium, short, long.
DECAYS = (0.9952, 0.962, 0.99931)
NAN = struct.pack("<Q", 0x7FF8000000000000)


def main(dashd, dashcli, source, out):
    data = bytearray(open(source, "rb").read())
    for decay in DECAYS:
        word = struct.pack("<d", decay)
        assert data.count(word) == 1, f"decay {decay} must occur once in the captured file"
        at = data.index(word)
        data[at : at + 8] = NAN
    datadir = tempfile.mkdtemp(prefix="im1a-nan-")
    os.chmod(datadir, 0o700)
    regtest = os.path.join(datadir, "regtest")
    os.mkdir(regtest, 0o700)
    with open(os.path.join(regtest, "fee_estimates.dat"), "wb") as f:
        f.write(data)
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    common = ["-regtest", f"-datadir={datadir}", f"-rpcport={port}"]
    node = subprocess.Popen(
        [dashd, *common, "-listen=0", "-discover=0", "-dnsseed=0", "-connect=0",
         "-disablewallet=1", "-server=1", "-printtoconsole=0"],
    )

    def cli(*args):
        r = subprocess.run([dashcli, *common, "-rpcwait", "-rpcwaittimeout=30", *args],
                           text=True, capture_output=True, timeout=40)
        if r.returncode:
            raise RuntimeError(r.stderr)
        return r.stdout.strip()

    try:
        try:
            json.loads(cli("getblockchaininfo"))
            cli("stop")
            assert node.wait(timeout=60) == 0
        finally:
            if node.poll() is None:
                node.terminate()
                try:
                    node.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    node.kill()
                    node.wait()
        log = open(os.path.join(regtest, "debug.log")).read()
        # fees.cpp: a refused file logs one of these (`Read` catches the exception and the
        # estimator starts from its defaults).
        for refusal in ("unable to read policy estimator data", "Failed to read fee estimates"):
            assert refusal not in log, "Core refused the file"
        written = open(os.path.join(regtest, "fee_estimates.dat"), "rb").read()
        # After a refusal the default decays would be written back, so this is the real check.
        assert written.count(NAN) == 3, "Core did not write the three NaN decays back"
    finally:
        shutil.rmtree(datadir, ignore_errors=True)
    with open(out, "wb") as f:
        f.write(written)
    print(f"wrote {out}: {len(written)} bytes, 3 NaN decays")


if __name__ == "__main__":
    main(*sys.argv[1:5])
