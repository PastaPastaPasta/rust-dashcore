#!/usr/bin/env python3
"""Capture dashd's -reindex-chainstate StateDigest replay of a regtest-v24 recording (CH5-1b-3).

For each recording's node0 under <root>/test-vectors/statedigest/regtest-v24:
  1. start the StateDigest-patched dashd on a fresh datadir with the recording's chain_args and
     mocktime, submit blocks.dat.zst's blocks and stop;
  2. restart with -coinstatsindex=0 -reindex-chainstate -statedigestlog=<work>/<recording>/core-replay.jsonl
     and stop at the recorded tip;
  3. check the replay's C records cover genesis through the tip, block for block with golden.jsonl.zst;
  4. reopen the frozen datadir with -coinstatsindex=1 -txindex=1, wait for both indexes to sync at
     the tip, and run the standard `digest extract` on the replay log;
  5. write replay-golden.jsonl.zst and replay-provenance.json beside the recording.

The rc.3 build refuses -coinstatsindex with -reindex-chainstate, hence the index-less replay; its
`masternode payments` RPC, which the extractor calls, needs -txindex=1.

A work directory that already holds a recording's datadir and replay log skips steps 1-2. Its
provenance then says "reconstructed": true: initial_command, replay_command and tip are rebuilt
from the arguments and the tip golden, not recorded from a start in this invocation.

The Core identity comes from `dashd -version`: its describe must end in -g<first 12 of --expect-commit>.
The build's branch, configure, depends and flags are taken as given.

Usage: capture-replay.py --root <rust-dashcore checkout with target/release/dash-conformance>
                         --dashd <StateDigest-patched dashd> --work <dir> --expect-commit <40-hex>
                         --branch=<b> --configure=<c> --depends=<d> --flags=<f> <recording>...
The build fields use the --name=value form, since their values start with "-".
"""
import argparse
import base64
import json
import re
import shutil
import struct
import subprocess
import time
from pathlib import Path
from urllib.request import Request, urlopen

from common import sha256

ROOT = BIN = CONF = OUT = TMP = CORE = None
SCRIPT = Path(__file__).resolve()


def rpc(url, method, params=[]):
    body = json.dumps({"jsonrpc": "1.0", "id": 1, "method": method, "params": params}).encode()
    req = Request(url, body, {"Authorization": "Basic " + base64.b64encode(b"mn0:mn0").decode(),
                              "Content-Type": "application/json"})
    with urlopen(req, timeout=60) as r:
        x = json.load(r)
    if x["error"] is not None:
        raise RuntimeError(f"{method}: {x['error']}")
    return x["result"]


def wait(url, method, params=[], want=None):
    error = None
    for _ in range(600):
        try:
            v = rpc(url, method, params)
            if want is None or v == want:
                return v
        except Exception as exc:
            error = exc
        time.sleep(1)
    raise TimeoutError(f"{method}: {error}")


def wait_coinstats(url, height):
    for _ in range(600):
        try:
            value = rpc(url, "getindexinfo")
            coinstats = value.get("coinstatsindex")
            txindex = value.get("txindex")
            if all(index and index.get("synced") and index.get("best_block_height") == height
                   for index in (coinstats, txindex)):
                return value
        except Exception:
            pass
        time.sleep(1)
    raise TimeoutError("getindexinfo: coinstatsindex and txindex did not sync")


def framed_blocks(path):
    raw = subprocess.check_output(["zstd", "-q", "-d", "-c", str(path)])
    at = 0
    blocks = []
    while at < len(raw):
        if len(raw) - at < 8:
            raise ValueError("short frame")
        magic = raw[at:at + 4]
        n = struct.unpack_from("<I", raw, at + 4)[0]
        at += 8
        b = raw[at:at + n]
        at += n
        if len(b) != n:
            raise ValueError("short block")
        blocks.append((magic, b))
    return blocks


def c_records(path):
    raw = subprocess.check_output(["zstd", "-q", "-d", "-c", str(path)]) if path.suffix == ".zst" else path.read_bytes()
    return [json.loads(line) for line in raw.splitlines() if json.loads(line).get("k") == "C"]


def validate_replay(path, tip_golden, blocks):
    want = c_records(tip_golden)
    seen = []
    for line in path.read_bytes().splitlines():
        record = json.loads(line)
        if record.get("k") == "C":
            seen.append(record)
    if len(seen) != len(blocks) or len(want) != len(blocks):
        raise RuntimeError(f"replay/tip C records {len(seen)}/{len(want)} != {len(blocks)}")
    for height, (record, expected) in enumerate(zip(seen, want)):
        if record.get("h") != height or record.get("b") != expected.get("b"):
            raise RuntimeError(f"replay C record mismatch at height {height}")


def command(datadir, port, args, mocktime, extra=[]):
    return [str(BIN), "-regtest", f"-datadir={datadir}", f"-rpcport={port}", "-server=1", "-rpcuser=mn0",
            "-rpcpassword=mn0", f"-mocktime={mocktime}", "-printtoconsole=0", "-listen=0"] + args + extra


def start(datadir, port, args, mocktime, extra=[]):
    cmd = command(datadir, port, args, mocktime, extra)
    log = datadir / "dashd-command.json"
    log.write_text(json.dumps(cmd, indent=2) + "\n")
    stderr = datadir / "dashd.stderr"
    p = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=stderr.open("w"), text=True)
    url = f"http://127.0.0.1:{port}"
    try:
        wait(url, "getblockcount")
    except Exception:
        p.kill()
        p.wait()
        raise RuntimeError(stderr.read_text())
    return p, url, cmd


def stop(p, url):
    try:
        rpc(url, "stop")
    finally:
        try:
            p.wait(timeout=600)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()


def core_identity(a):
    if not re.fullmatch(r"[0-9a-f]{40}", a.expect_commit):
        raise SystemExit("--expect-commit must be a full 40-hex commit")
    version = subprocess.run([str(a.dashd), "-version"], capture_output=True, text=True, check=True)
    first = version.stdout.splitlines()[0]
    m = re.fullmatch(r"Dash Core version (\S+)", first.strip())
    if not m:
        raise SystemExit(f"unrecognized dashd -version output: {first!r}")
    describe = m.group(1)
    if not describe.endswith(f"-g{a.expect_commit[:12]}"):
        raise SystemExit(f"dashd describe {describe} is not --expect-commit {a.expect_commit}")
    return {"describe": describe, "commit": a.expect_commit, "branch": a.branch, "configure": a.configure,
            "depends": a.depends, "flags": a.flags}


def capture(name):
    src = OUT / name / "node0"
    rec = json.loads((src / "extract.json").read_text())
    args = list(rec["chain_args"])
    assert args == rec["chain_args"], "chain_args must be the recording values"
    blocks = framed_blocks(src / "blocks.dat.zst")
    height = len(blocks) - 1
    mocktime = int(rec["mocktime"])
    port = 30000 + (sum(map(ord, name)) % 10000)
    run = TMP / name
    replay = run / "core-replay.jsonl"
    core = run / "core"
    reuse = run.exists() and core.exists() and replay.exists()
    if not reuse:
        shutil.rmtree(run, ignore_errors=True)
        run.mkdir(mode=0o700)
        core.mkdir(mode=0o700)
    flags = ["-txindex=0", "-disablewallet", "-connect=0", "-par=1", "-assumevalid=0"]
    if reuse:
        initial_cmd = command(core, port, args + flags, mocktime)
        replay_cmd = command(core, port, args + flags, mocktime,
                             ["-coinstatsindex=0", "-reindex-chainstate", f"-statedigestlog={replay}"])
        tip = c_records(src / "golden.jsonl.zst")[-1]["b"]
    else:
        p, url, initial_cmd = start(core, port, args + flags, mocktime)
        for i, (_, b) in enumerate(blocks[1:], 1):
            r = rpc(url, "submitblock", [b.hex()])
            if r is not None:
                raise RuntimeError(f"{name} submit {i}: {r}")
        tip = rpc(url, "getbestblockhash", [])
        stop(p, url)
        p, url, replay_cmd = start(core, port, args + flags, mocktime,
                                   ["-coinstatsindex=0", "-reindex-chainstate", f"-statedigestlog={replay}"])
        wait(url, "getbestblockhash", [], tip)
        stop(p, url)
    if not replay.exists():
        raise RuntimeError("missing replay log")
    digest_lines = replay.read_bytes().splitlines()
    if len(digest_lines) < height + 1:
        raise RuntimeError(f"replay lines {len(digest_lines)} < {height + 1}")
    validate_replay(replay, src / "golden.jsonl.zst", blocks)
    p, url, reopen_cmd = start(core, port,
                               args + ["-txindex=1", "-disablewallet", "-connect=0", "-par=1", "-assumevalid=0"],
                               mocktime, ["-coinstatsindex=1"])
    wait_coinstats(url, height)
    golden = run / "core-golden.jsonl"
    summary = run / "extract-summary.json"
    ex = subprocess.run([str(CONF), "digest", "extract", "--rpc-url", url, "--rpc-user", "mn0", "--rpc-password",
                         "mn0", "--dense", "--digest", str(replay), "--out", str(golden), "--blocks-dir",
                         str(core / "regtest/blocks"), "--summary", str(summary)],
                        capture_output=True, text=True, timeout=1800)
    if ex.returncode:
        raise RuntimeError("extract failed\n" + ex.stdout + "\n" + ex.stderr)
    stop(p, url)
    target = src / "replay-golden.jsonl.zst"
    tmpgold = run / "replay-golden.jsonl.zst"
    subprocess.run(["zstd", "-q", "-19", "-f", str(golden), "-o", str(tmpgold)], check=True)
    shutil.copy2(tmpgold, target)
    files = {
        "blocks.dat.zst": sha256(src / "blocks.dat.zst"),
        "replay-golden.jsonl.zst": sha256(target),
        "replay.jsonl": sha256(replay),
        "extract-summary.json": sha256(summary),
    }
    prov = {
        "recording": name, "node": "node0", "blocks": height + 1, "tip_height": height, "tip": tip,
        "mocktime": mocktime,
        "core": {"binary": str(BIN), "sha256": sha256(BIN), **CORE},
        "capture_script": {"path": str(SCRIPT.relative_to(SCRIPT.parents[3])), "sha256": sha256(SCRIPT)},
        "chain_args": args, "g11_args": flags, "initial_command": initial_cmd, "replay_command": replay_cmd,
        "replay_index_note": "The rc.3 build refuses coinstatsindex with -reindex-chainstate; replay uses "
                             "-coinstatsindex=0. Reopen uses -coinstatsindex=1 -txindex=1 because the rc.3 "
                             "masternode payments RPC used by the extractor requires txindex=1; v24.0.0 "
                             "returns an RPC error instead.",
        "reopen_command": reopen_cmd,
        "extract_command": [str(CONF), "digest", "extract", "--rpc-url", "<port>", "--rpc-user", "mn0",
                            "--rpc-password", "mn0", "--dense", "--digest", "core-replay.jsonl", "--out",
                            "core-golden.jsonl", "--blocks-dir", "regtest/blocks", "--summary",
                            "extract-summary.json"],
        "files": files,
        "sizes": {k: (src / k).stat().st_size if (src / k).exists() else None
                  for k in ["blocks.dat.zst", "replay-golden.jsonl.zst"]},
        "replay_log_lines": len(digest_lines), "extract_summary": json.loads(summary.read_text()),
    }
    if reuse:
        prov["reconstructed"] = True
    (src / "replay-provenance.json").write_text(json.dumps(prov, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"name": name, "blocks": height + 1, "golden_bytes": target.stat().st_size,
                      "replay_bytes": replay.stat().st_size, "tip": tip}, indent=2))


def main():
    global ROOT, BIN, CONF, OUT, TMP, CORE
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--dashd", type=Path, required=True)
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--expect-commit", required=True)
    for build_field in ("--branch", "--configure", "--depends", "--flags"):
        ap.add_argument(build_field, required=True)
    ap.add_argument("recordings", nargs="+")
    a = ap.parse_args()
    ROOT, BIN, TMP = a.root, a.dashd, a.work
    CORE = core_identity(a)
    CONF = ROOT / "target/release/dash-conformance"
    OUT = ROOT / "test-vectors/statedigest/regtest-v24"
    TMP.mkdir(mode=0o700, exist_ok=True)
    for name in a.recordings:
        capture(name)


if __name__ == "__main__":
    main()
