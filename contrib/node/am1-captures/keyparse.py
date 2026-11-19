#!/usr/bin/env python3
"""Record AM1 operator-key parsing and constructor logs with func-compat's v24 launcher.

Only public, deterministic test keys and the pre-generated network.json test keys belong here.
The Core tree is read-only; exported framework files, binaries and datadirs stay under --work.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[3]
INVALID = "Invalid masternodeblsprivkey. Please see documentation."
VALID = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20"
ORDER = "73eda753299d7d483339d80809a1d80553bda402fffe5bfeffffffff00000001"
PUBLIC = re.compile(r"MASTERNODE:\n  blsPubKeyOperator legacy: ([0-9a-f]{96})\n  blsPubKeyOperator basic: ([0-9a-f]{96})")


def launcher():
    path = ROOT / "contrib/node/func-compat/func-compat.py"
    spec = importlib.util.spec_from_file_location("am1_func_compat", path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def rows():
    values = [
        ("null", "null"), ("false", "false"), ("true", "true"),
        ("integer", "1234"), ("number-text", "1.50"),
        ("number-valid-scalar", "1" * 64),
        ("array", "[]"), ("object", "{}"), ("empty", '""'),
        ("odd", '"123"'), ("single-zero-byte", '"00"'),
        ("zero-key", json.dumps("00" * 32)),
        ("short-key", json.dumps("01" * 31)),
        ("long-key", json.dumps("01" * 33)),
        ("group-order", json.dumps(ORDER)),
        ("order-plus-one", json.dumps(f"{int(ORDER, 16) + 1:064x}")),
        ("above-order", json.dumps("ff" * 32)),
        ("order-minus-one", json.dumps(f"{int(ORDER, 16) - 1:064x}")),
        ("valid", json.dumps(VALID)), ("uppercase", json.dumps(VALID.upper())),
        ("whitespace", json.dumps(" \t" + " \n".join(VALID[i:i + 2] for i in range(0, 64, 2)) + "\r\v\f")),
        ("whitespace-only", '" \\t\\n\\r\\u000b\\f"'),
        ("split-nibble", json.dumps(VALID[:1] + " " + VALID[1:])),
        ("odd-after-valid", json.dumps(VALID + "0")),
        ("bad-after-valid", json.dumps(VALID + "zz")),
        ("hex-prefix", json.dumps("0x" + VALID)),
        ("non-ascii-space", json.dumps("\u00a0" + VALID)),
    ]
    for name, value_json in values:
        yield {"name": name, "source": "settings", "value_json": value_json}
        value = json.loads(value_json)
        if isinstance(value, str):
            yield {"name": name, "source": "command-line", "argv": [f"-masternodeblsprivkey={value}"]}
    for name, argv in [
        ("absent", []), ("elided", ["-masternodeblsprivkey"]),
        ("negated", ["-nomasternodeblsprivkey"]),
        ("last-empty", [f"-masternodeblsprivkey={VALID}", "-masternodeblsprivkey="]),
        ("last-valid", ["-masternodeblsprivkey=", f"-masternodeblsprivkey={VALID}"]),
    ]:
        yield {"name": name, "source": "command-line", "argv": argv}
    for name, value in [("valid", VALID), ("empty", ""), ("invalid", "00")]:
        yield {"name": name, "source": "config", "value": value}


def record(binary, work, index, row):
    datadir = Path(tempfile.mkdtemp(prefix=f"node-{index:03}-", dir=work))
    argv = [str(binary), f"-datadir={datadir}", "-regtest", "-disablewallet", "-listen=0",
            "-listenonion=0", "-server=0", "-dnsseed=0", "-fixedseeds=0", "-connect=0",
            "-par=1", "-parbls=1", "-dbcache=4", "-maxsigcachesize=1", "-printtoconsole=1",
            "-logtimestamps=0", "-logthreadnames=0", "-stopafterblockimport=1"]
    if row["source"] == "settings":
        (datadir / "regtest").mkdir(mode=0o700)
        (datadir / "regtest/settings.json").write_text('{"masternodeblsprivkey":' + row["value_json"] + '}\n')
    elif row["source"] == "config":
        (datadir / "dash.conf").write_text("masternodeblsprivkey=" + row["value"] + "\n")
    else:
        argv.extend(row["argv"])
    run = subprocess.run(argv, capture_output=True, text=True, timeout=120)
    text = run.stdout + run.stderr
    (datadir / "capture.txt").write_text(text)
    public = PUBLIC.search(text)
    exception = re.search(r"JSON value of type (?:array|object) is not of expected type string", text)
    if INVALID in text:
        result = {"outcome": "invalid", "diagnostic": INVALID}
    elif exception:
        result = {"outcome": "exception", "diagnostic": exception.group()}
    elif public:
        result = {"outcome": "key", "legacy": public[1], "basic": public[2], "log": public[0] + "\n"}
    elif "Stopping after block import" in text:
        result = {"outcome": "inactive"}
    else:
        raise RuntimeError(f"unexpected result for {row['source']}/{row['name']}; see {datadir}/capture.txt")
    return {**row, **result, "exit_code": run.returncode}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--cache", required=True, type=Path)
    parser.add_argument("--network", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=ROOT / "dashd-masternode/tests/fixtures/am1-key/core-v24.json")
    args = parser.parse_args()
    args.work.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.environ["RUST_DASHCORE_TEST_CACHE"] = str(args.cache.resolve())
    fc = launcher()
    tree = fc.build(args.core, args.work / "launcher", "v24.0", False, branch="v24.0.0")
    binary = (tree / "build/src/dashd").resolve()
    version = subprocess.check_output([binary, "-version"], text=True).splitlines()[0]
    if version != "Dash Core version v24.0.0":
        raise RuntimeError(f"wrong binary: {version}")
    captured = []
    for i, row in enumerate(rows()):
        captured.append(record(binary, args.work, i, row))
        print(f"{row['source']}/{row['name']}: {captured[-1]['outcome']}", flush=True)
    network = json.loads(args.network.read_text())
    keys = []
    for mn in network["masternodes"]:
        row = {"name": f"network-mn-{mn['index']}", "source": "command-line",
               "argv": [f"-masternodeblsprivkey={mn['bls_private_key']}"]}
        keys.append(record(binary, args.work, len(captured) + len(keys), row))
        if keys[-1]["outcome"] != "key":
            raise RuntimeError("network key was refused")
        keys[-1]["index"] = mn["index"]
        keys[-1]["secret"] = mn["bls_private_key"]
        keys[-1]["network_public"] = mn["bls_public_key"]
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps({
        "version": version,
        "core_commit": fc.git(args.core, "rev-parse", "v24.0.0^{commit}").strip(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "network_sha256": hashlib.sha256(args.network.read_bytes()).hexdigest(),
        "rows": captured, "network_keys": keys,
    }, indent=2, ensure_ascii=True) + "\n")
    print(args.out)


if __name__ == "__main__":
    main()
