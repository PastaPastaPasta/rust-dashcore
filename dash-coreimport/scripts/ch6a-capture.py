#!/usr/bin/env python3
"""Capture CH6a's stopped v24.0.0 regtest LevelDB fixtures, using only stdlib and RPC.

Run from the repository root after contrib/setup-dashd.py with DASHVERSION=24.0.0.
All live datadirs belong to --work (0700); --out must not already exist.
The small format decoder below audits placement; it never opens a LevelDB library.
"""

import argparse
import base64
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
import urllib.error
import urllib.request

KEYS = [b"B", b"R", b"l", b"F\x10prunedblockfiles", b"\x0e\0obfuscate_key"]
ADDRESS = "yVg3NBUHNEhgDceqwVUjsZHreC5PBHnUo9"


def varint(data, pos):
    value = shift = 0
    while True:
        byte = data[pos]
        pos += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, pos
        shift += 7
        assert shift < 70


def sized(data, pos):
    size, pos = varint(data, pos)
    return data[pos:pos + size], pos + size


def records(path):
    data = path.read_bytes()
    assembled = bytearray()
    start = None
    for base in range(0, len(data), 32768):
        end = min(base + 32768, len(data))
        pos = base
        while pos + 7 <= end:
            _, size, kind = struct.unpack_from("<IHB", data, pos)
            if kind == 0 and size == 0:
                assert not any(data[pos:end])
                break
            assert pos + 7 + size <= end
            payload = data[pos + 7:pos + 7 + size]
            if kind == 1:
                assert start is None
                yield payload, pos + 7
            elif kind == 2:
                assert start is None
                start = pos + 7
                assembled = bytearray(payload)
            else:
                assert start is not None and kind in (3, 4)
                assembled.extend(payload)
                if kind == 4:
                    yield bytes(assembled), start
                    start = None
            pos += 7 + size
        assert not any(data[pos:end]) or pos == end
    assert start is None


def block_entries(data, offset):
    count = struct.unpack_from("<I", data, len(data) - 4)[0]
    end = len(data) - 4 * (count + 1)
    key = b""
    pos = 0
    while pos < end:
        at = offset + pos
        shared, pos = varint(data, pos)
        size, pos = varint(data, pos)
        value_len, pos = varint(data, pos)
        key = key[:shared] + data[pos:pos + size]
        pos += size
        value = data[pos:pos + value_len]
        pos += value_len
        yield key, value, at
    assert pos == end


def physical_offset(start, logical):
    first = 32768 - start % 32768
    if logical < first:
        return start + logical
    remaining = logical - first
    return (start // 32768 + 1 + remaining // 32761) * 32768 + 7 + remaining % 32761


def table_records(path, features):
    data = path.read_bytes()
    footer = len(data) - 48
    meta_offset, pos = varint(data, footer)
    meta_size, pos = varint(data, pos)
    index_offset, pos = varint(data, pos)
    index_size, _ = varint(data, pos)
    for key, value, at in block_entries(data[meta_offset:meta_offset + meta_size], meta_offset):
        features.append({"file": str(path), "offset": at, "metadata": key.decode()})
        offset, p = varint(value, 0)
        size, _ = varint(value, p)
        assert data[offset + size] == 0
    for _, value, _ in block_entries(data[index_offset:index_offset + index_size], index_offset):
        offset, p = varint(value, 0)
        size, _ = varint(value, p)
        assert data[offset + size] == 0
        for key, value, at in block_entries(data[offset:offset + size], offset):
            tag = struct.unpack_from("<Q", key, len(key) - 8)[0]
            yield key[:-8], tag >> 8, tag & 255, value, at


def audit(db):
    manifest = db / (db / "CURRENT").read_text().strip()
    tables = {}
    features = []
    log_number = prev_log = 0
    for record, offset in records(manifest):
        pos = 0
        while pos < len(record):
            at = physical_offset(offset, pos)
            tag, pos = varint(record, pos)
            feature = {"file": str(manifest), "offset": at, "manifest_tag": tag}
            if tag == 1:
                value, pos = sized(record, pos)
                feature["comparator"] = value.decode()
            elif tag in (2, 3, 4, 9):
                value, pos = varint(record, pos)
                if tag == 2:
                    log_number = value
                if tag == 9:
                    prev_log = value
            elif tag in (5, 6, 7):
                level, pos = varint(record, pos)
                if tag == 5:
                    _, pos = sized(record, pos)
                else:
                    number, pos = varint(record, pos)
                    if tag == 6:
                        tables.pop((level, number), None)
                    else:
                        _, pos = varint(record, pos)
                        _, pos = sized(record, pos)
                        _, pos = sized(record, pos)
                        tables[(level, number)] = db / f"{number:06}.ldb"
            else:
                raise AssertionError(f"unlisted manifest tag {tag}: {manifest}:{at}")
            features.append(feature)
    versions = []
    for (level, _), path in tables.items():
        for key, seq, kind, value, at in table_records(path, features):
            if key in KEYS:
                versions.append(dict(key=key.hex(), sequence=seq, kind=kind, value=value.hex(),
                                     file=str(path), offset=at, level=level))
    for path in sorted(db.glob("*.log")):
        number = int(path.stem)
        if number < log_number and number != prev_log:
            continue
        for record, offset in records(path):
            sequence, count = struct.unpack_from("<QI", record)
            pos = 12
            for n in range(count):
                at = physical_offset(offset, pos)
                kind = record[pos]
                key, pos = sized(record, pos + 1)
                value = b""
                if kind == 1:
                    value, pos = sized(record, pos)
                else:
                    assert kind == 0
                if key in KEYS:
                    versions.append(dict(key=key.hex(), sequence=sequence + n, kind=kind,
                                         value=value.hex(), file=str(path), offset=at, level="wal"))
            assert pos == len(record)
    return dict(versions=versions, levels=sorted({level for level, _ in tables}), features=features)


def latest(audit_result, key):
    logs = sorted((v for v in audit_result["versions"] if v["level"] == "wal"),
                  key=lambda v: (v["file"], v["offset"]))
    if any(a["sequence"] >= b["sequence"] for a, b in zip(logs, logs[1:])):
        raise ValueError("log sequence order")
    values = [v for v in audit_result["versions"] if v["key"] == key.hex()]

    def priority(value):
        level = -1 if value["level"] == "wal" else value["level"]
        number = int(Path(value["file"]).stem) if level <= 0 else 0
        return level, -number, -value["sequence"]

    values.sort(key=priority)
    if values and any(v["sequence"] > values[0]["sequence"] for v in values[1:]):
        raise ValueError("source priority contradiction")
    return values[0] if values else None


def best_hash(audit_result):
    best = latest(audit_result, b"B")
    if best is None or best["kind"] == 0:
        return None
    key = latest(audit_result, b"\x0e\0obfuscate_key")
    xor = bytes.fromhex(key["value"])[1:] if key else bytes(8)
    value = bytes.fromhex(best["value"])
    return bytes(b ^ xor[n % 8] for n, b in enumerate(value))[::-1].hex()


class Node:
    def __init__(self, binary, root, args, commands):
        root.mkdir(mode=0o700, parents=True)
        self.root, self.binary, self.extra, self.commands = root, binary, args, commands
        self.process = None
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]

    def rpc(self, method, *params):
        auth = base64.b64encode(b"ch6a:regtest-fixture").decode()
        request = urllib.request.Request(f"http://127.0.0.1:{self.port}/",
                                        json.dumps(dict(id=1, method=method, params=params)).encode(),
                                        {"Authorization": f"Basic {auth}", "Content-Type": "application/json"})
        try:
            response = urllib.request.urlopen(request, timeout=60)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            result = json.load(response)
        if result["error"]:
            raise RuntimeError(result["error"])
        return result["result"]

    def start(self, *extra, wait=True):
        args = [self.binary, f"-datadir={self.root}", "-regtest", "-server=1", "-listen=0",
                "-connect=0", "-dnsseed=0", "-discover=0", "-listenonion=0", "-upnp=0", "-natpmp=0",
                "-disablewallet=1", "-dbcache=4", "-par=1", "-rpcbind=127.0.0.1",
                "-rpcallowip=127.0.0.1", f"-rpcport={self.port}", "-rpcuser=ch6a",
                "-rpcpassword=regtest-fixture", *self.extra, *extra]
        self.commands.append(args)
        self.process = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if wait:
            deadline = time.monotonic() + 120
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise RuntimeError((self.root / "regtest/debug.log").read_text()[-4000:])
                try:
                    self.rpc("getblockcount")
                    return
                except (OSError, RuntimeError):
                    time.sleep(0.05)
            raise TimeoutError("dashd RPC startup")

    def stop(self, kill=False):
        if self.process and self.process.poll() is None:
            if kill:
                self.process.kill()
            else:
                self.rpc("stop")
            self.process.wait(timeout=120)

    def mine(self, count):
        for n in range(0, count, 100):
            self.rpc("generatetoaddress", min(100, count - n), ADDRESS)


def capture(node, output, case, expected):
    assert node.process.poll() is not None
    source = node.root / "regtest"
    dest = output / case
    for subdir in ("blocks/index", "chainstate"):
        (dest / subdir).mkdir(parents=True)
        for path in (source / subdir).iterdir():
            if path.name in ("CURRENT",) or path.name.startswith("MANIFEST-") or path.suffix in (".ldb", ".sst", ".log"):
                shutil.copy2(path, dest / subdir / path.name)
    for path in (source / "blocks").glob("blk?????.dat"):
        with path.open("rb") as file:
            data = file.read()
        pos = 0
        while data[pos:pos + 4] == bytes.fromhex("fcc1b7dc"):
            size = struct.unpack_from("<I", data, pos + 4)[0]
            assert 80 <= size <= 2000000 and pos + 8 + size <= len(data)
            pos += 8 + size
        assert not any(data[pos:])
        (dest / "blocks" / path.name).write_bytes(data[:pos])
    audits = {name: audit(dest / name) for name in ("chainstate", "blocks/index")}
    assert best_hash(audits["chainstate"]) == expected["best_block"]
    key = latest(audits["chainstate"], b"\x0e\0obfuscate_key")
    key = bytes.fromhex(key["value"])
    assert len(key) == 9 and key[0] == 8 and any(key[1:])
    logged_keys = re.findall(r"Using obfuscation key for " + re.escape(str(source / "chainstate")) + r": ([0-9a-f]{16})", (source / "debug.log").read_text())
    assert logged_keys and logged_keys[-1] == key[1:].hex()
    last_file = latest(audits["blocks/index"], b"l")
    last_file = int.from_bytes(bytes.fromhex(last_file["value"]), "little") if last_file else None
    if last_file is not None:
        assert last_file == max(int(p.stem[3:]) for p in (dest / "blocks").glob("blk?????.dat"))
    expected["decoded"] = dict(R=expected["reindexing"], prunedblockfiles=expected["pruned"],
                               last_file=last_file, obfuscation_key=key[1:].hex(), B=expected["best_block"])
    for result in audits.values():
        for item in result["versions"] + result["features"]:
            item["file"] = str(Path(item["file"]).relative_to(output))
    expected["audit"] = audits
    (dest / "expected.json").write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
    print(f"captured {case}: tip {expected['best_block']}, levels {audits['chainstate']['levels']}", flush=True)
    return audits


def crc32c(data):
    crc = 0xffffffff
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82f63b78 if crc & 1 else 0)
    crc ^= 0xffffffff
    return (((crc >> 15) | (crc << 17)) + 0xa282ead8) & 0xffffffff


def mutations(output):
    for name, original in (("torn-wal", "a-wal"), ("bad-crc", "b-l0"), ("snappy", "b-l0")):
        dest = output / name
        shutil.copytree(output / original, dest)
        if name == "torn-wal":
            path = next(p for p in (dest / "chainstate").glob("*.log") if p.stat().st_size)
            path.write_bytes(path.read_bytes()[:-1])
            offset = path.stat().st_size
        else:
            result = json.loads((dest / "expected.json").read_text())
            item = next(v for v in result["audit"]["chainstate"]["versions"]
                        if v["key"] == b"B".hex() and v["level"] == 0 and v["kind"] == 1)
            path = dest / "chainstate" / Path(item["file"]).name
            data = bytearray(path.read_bytes())
            footer = len(data) - 48
            _, pos = varint(data, footer)
            _, pos = varint(data, pos)
            index, pos = varint(data, pos)
            size, _ = varint(data, pos)
            _, value, _ = next(block_entries(data[index:index + size], index))
            block, pos = varint(value, 0)
            size, _ = varint(value, pos)
            offset = block if name == "bad-crc" else block + size
            data[offset] ^= 1
            if name == "snappy":
                assert data[offset] == 1
                struct.pack_into("<I", data, offset + 1, crc32c(data[block:offset + 1]))
            path.write_bytes(data)
        (dest / "mutation.json").write_text(json.dumps(dict(file=str(path.relative_to(output)), offset=offset, mutation=name), indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    binary = os.environ["DASHD_PATH"]
    version = subprocess.check_output([binary, "--version"], text=True).splitlines()[0]
    assert version == "Dash Core version v24.0.0", version
    args.work.mkdir(mode=0o700, parents=True, exist_ok=False)
    args.out.mkdir(parents=True, exist_ok=False)
    commands, nodes = [], []
    try:
        normal = Node(binary, args.work / "normal", [], commands)
        nodes.append(normal)
        normal.start()
        normal.mine(8)
        genesis = normal.rpc("getblockhash", 0)
        tip = normal.rpc("getbestblockhash")
        normal.stop()
        expected = dict(genesis=genesis, best_block=tip, pruned=False, reindexing=False)
        a = capture(normal, args.out, "a-wal", dict(expected))
        assert latest(a["chainstate"], b"B")["level"] == "wal"
        normal.start()
        assert normal.rpc("getbestblockhash") == tip
        normal.stop()
        b = capture(normal, args.out, "b-l0", dict(expected))
        assert latest(b["chainstate"], b"\x0e\0obfuscate_key")["level"] == 0
        assert any(v["key"] == b"B".hex() and v["level"] == 0 for v in b["chainstate"]["versions"])
        normal.start()
        normal.mine(1)
        tip = normal.rpc("getbestblockhash")
        expected["best_block"] = tip
        normal.stop()
        c = capture(normal, args.out, "c-overwrite", dict(expected))
        assert {v["level"] for v in c["chainstate"]["versions"] if v["key"] == b"B".hex()} >= {0, "wal"}
        normal.start("-forcecompactdb")
        normal.mine(1)
        expected["best_block"] = normal.rpc("getbestblockhash")
        normal.stop()
        normal.start()
        normal.stop()
        e = capture(normal, args.out, "e-multilevel", dict(expected))
        assert 0 in e["chainstate"]["levels"] and any(level > 0 for level in e["chainstate"]["levels"])

        normal.start()
        normal.mine(1500)
        expected["best_block"] = normal.rpc("getbestblockhash")
        normal.stop()
        debug = normal.root / "regtest/debug.log"
        before = debug.stat().st_size
        normal.start("-reindex", wait=False)
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            if normal.process.poll() is not None:
                raise RuntimeError("reindex process exited")
            with debug.open("rb") as file:
                file.seek(before)
                text = file.read()
            if b"Reindexing block file" in text:
                os.kill(normal.process.pid, signal.SIGSTOP)
                normal.stop(kill=True)
                break
            time.sleep(0.001)
        else:
            raise TimeoutError("mid-reindex capture")
        stopped_tip = best_hash(audit(normal.root / "regtest/chainstate"))
        assert stopped_tip in (None, expected["best_block"])
        g = capture(normal, args.out, "g-reindex", dict(expected, best_block=stopped_tip, reindexing=True))
        assert latest(g["blocks/index"], b"R")["kind"] == 1
        normal.start()
        deadline = time.monotonic() + 120
        while normal.rpc("getbestblockhash") != expected["best_block"]:
            if time.monotonic() > deadline:
                raise TimeoutError("reindex completion")
            time.sleep(0.05)
        normal.stop()
        d = capture(normal, args.out, "d-deleted", dict(expected))
        r = latest(d["blocks/index"], b"R")
        assert r["kind"] == 0
        assert any(v["key"] == b"R".hex() and v["kind"] == 1 and v["level"] != "wal" for v in d["blocks/index"]["versions"])

        pruned = Node(binary, args.work / "pruned", ["-prune=1", "-fastprune"], commands)
        nodes.append(pruned)
        pruned.start()
        pruned.mine(700)
        pruned.rpc("pruneblockchain", 400)
        info = pruned.rpc("getblockchaininfo")
        assert info["pruned"] and info["pruneheight"] > 0
        tip = pruned.rpc("getbestblockhash")
        pruned.stop()
        f = capture(pruned, args.out, "f-pruned", dict(expected, best_block=tip, pruned=True))
        assert bytes.fromhex(latest(f["blocks/index"], b"F\x10prunedblockfiles")["value"]) == b"1"
        mutations(args.out)
        subprocess.run([sys.executable, str(Path(__file__).with_name("ch6a-derive.py")), str(args.out)], check=True)
        (args.out / ".gitignore").write_text("!*.log\n")
        inventory = {str(path.relative_to(args.out)): hashlib.sha256(path.read_bytes()).hexdigest()
                     for path in sorted(args.out.rglob("*")) if path.is_file()}
        (args.out / "SHA256SUMS.json").write_text(json.dumps(inventory, indent=2, sort_keys=True) + "\n")
        provenance = f"""# CH6a LevelDB fixture provenance

Reference: `{version}`; binary SHA256 `{hashlib.sha256(Path(binary).read_bytes()).hexdigest()}`.

Captured by `dash-coreimport/scripts/ch6a-capture.py`, using fresh regtest datadirs
created 0700 and private ephemeral RPC ports. The script requires the exact final
v24.0.0 binary supplied by `DASHVERSION=24.0.0 contrib/setup-dashd.py`.

Reproduce from the repository root:

```sh
export DASHVERSION=24.0.0
eval $(python3 contrib/setup-dashd.py)
python3 dash-coreimport/scripts/ch6a-capture.py --work=/work/scratch/ch6a-capture-UNIQUE --out=/work/scratch/ch6a-fixtures-UNIQUE
```

Steps: (a) mine 8 blocks and stop; (b) restart and stop, so Core recovery creates
an L0 table (the obfuscation key is table-only; B is also rewritten on startup
and shutdown); (c) restart, mine one block, stop, leaving a newer B in the WAL;
(e) restart with -forcecompactdb, mine one block, stop, then restart and stop,
leaving an L0 table alongside a deeper table; (g) mine 1500 more blocks, stop,
start -reindex and SIGSTOP/SIGKILL at the first Reindexing block file log message;
(d) resume that reindex and stop, leaving a newer R tombstone above its recovered
table value (the tombstone may itself flush into L0); (f) fresh -prune=1 -fastprune node, mine 700 blocks,
pruneblockchain(400), getblockchaininfo confirms pruneheight > 0, stop.

Every best_block and genesis is RPC-known, recorded before stopping. For (g),
the reindex wipe may leave B absent; the capture asserts B is either absent or
the RPC-known pre-reindex tip. All processes are stopped before copying.

Only CURRENT, manifests, tables and WALs are copied, verbatim; no LOCK or LOG.
Every blk record is retained verbatim; only the known zero-filled preallocation
tail is omitted. All headers, including the RPC-known best block, remain available
for complete source inspection. No original DB is modified.
The audit in each expected.json records actual live levels, versions, manifest
tags, comparator and Bloom metadata with source byte offsets. Metadata is opaque
to the point reader; its block checksum is checked. SHA256SUMS.json covers captures.

Mutations are copies: torn-wal removes the final byte of a nonempty chainstate
WAL; bad-crc flips the first data-block byte of the B table; snappy changes its
compression type to 1 and recomputes the checksum, isolating compression refusal.
Each mutation.json records the changed file and byte offset.

The amended fixtures are generated by `scripts/ch6a-derive.py` from these stopped
captures. `h-nonzero-key` preserves a fresh random nonzero key checked against
Core's debug log. `i-multiple-logs` adds live logs with deliberately inverted file
and sequence order. `j-l0-older-tombstone`, `k-l0-older-value` and
`l-lower-level-tombstone` reconstruct tables/manifests with RPC-known captured
values and adversarial cross-level sequences. Those layouts are constructed
parser fixtures, not claims that Core produced such a compaction state.
Their `derivation.json` files record the exact rows/sequences and base capture.
Malformed record values, absent B/key, a B outside the block files, torn framing,
bad framing, malformed block payload, and corruption in a later blk file are
separate derived copies. No mutation or construction touches a source datadir.
The decoded values in ordinary captures are asserted against RPC state, file
numbering and the obfuscation key logged by dashd, separately from raw key audits.

Exact invocation arguments (capture-specific paths/ports are incidental):

```json
{json.dumps(commands, indent=2)}
```
"""
        (args.out / "PROVENANCE-leveldb.md").write_text(provenance)
    finally:
        for node in nodes:
            node.stop(kill=True)


if __name__ == "__main__":
    main()
