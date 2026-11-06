"""Captures dashd's `CoreRead` clear for crafted `netfulfilled.dat`, `mncache.dat` and `sporks.dat`
(SP1 D17).

Each case is a checksum-valid regtest file in Core's `CFlatDB` format whose object stops at one
compact size: over `MAX_SIZE` (serialize.h:38-42), exactly `MAX_SIZE` with nothing after it, or
non-canonical. dashd loads the case's two files at startup; the tool writes each file and dashd's
`CoreRead` line for it, so `dashd-masternode/tests/core_flatfiles.rs` can read the same object
with SP1's codec and compare the line.

  P2A_DASHD=/path/to/dashd-24.0.0-rc.4/bin/dashd clear_vectors.py OUT_DIR

Ports: 21900 and up (below the ephemeral range).
"""
import hashlib
import os
import re
import struct
import subprocess
import sys
import tempfile
import time

REGTEST = bytes.fromhex("fcc1b7dc")
MAX_SIZE = 0x02000000
META_VERSION = b"CMasternodeMetaMan-Version-5"


def cs(n):
    """A compact size in its shortest form."""
    if n < 253:
        return bytes([n])
    if n <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", n)
    if n <= 0xFFFFFFFF:
        return b"\xfe" + struct.pack("<I", n)
    return b"\xff" + struct.pack("<Q", n)


def s(b):
    return cs(len(b)) + b


def flat(magic, body, raw_magic=None):
    data = (raw_magic if raw_magic is not None else s(magic)) + REGTEST + body
    return data + hashlib.sha256(hashlib.sha256(data).digest()).digest()


# One `CService`: `::ffff:127.0.0.1` and a big-endian port.
SERVICE = bytes(10) + b"\xff\xff" + bytes([127, 0, 0, 1]) + struct.pack(">H", 19899)
# One `CMasternodeMetaInfo` up to its vote map: protx hash, m_last_dsq, m_mixing_tx_count.
INFO_HEAD = bytes(range(32)) + struct.pack("<qi", 7, 1)
# One `mapSporksByHash` entry up to its signature: the version string, a count of 1, a hash key,
# and `CSporkMessage`'s nSporkID (SPORK_2), nValue and nTimeSigned.
SPORK_HEAD = (s(b"CSporkManager-Version-3") + cs(1) + bytes(32)
              + struct.pack("<iqq", 10001, 0, 1700000000))
SP_MAGIC = b"magicSporkCache"
NF_MAGIC = b"magicFulfilledCache"
MN_MAGIC = b"magicMasternodeCache"

# case: (file, magic, body[, raw magic]). Each object stops at the compact size the name gives;
# `nf-short` is a file shorter than its hash (magic None: the body is the whole file).
CASES = {
    "nf-short": ("netfulfilled.dat", None, b"\x01" * 10),
    "nf-magic": ("netfulfilled.dat", NF_MAGIC, b"", b"\xfe" + struct.pack("<I", MAX_SIZE + 1)),
    "nf-outer": ("netfulfilled.dat", NF_MAGIC, cs(MAX_SIZE + 1)),
    "nf-outer-max": ("netfulfilled.dat", NF_MAGIC, cs(MAX_SIZE)),
    "nf-outer-noncanonical": ("netfulfilled.dat", NF_MAGIC, b"\xfd\xfc\x00"),
    # The 9-byte form: 5, and MAX_SIZE + 1, both under 2^32, so non-canonical before too large.
    "nf-outer-noncanonical-9": ("netfulfilled.dat", NF_MAGIC, b"\xff" + struct.pack("<Q", 5)),
    "nf-outer-over-9": ("netfulfilled.dat", NF_MAGIC, b"\xff" + struct.pack("<Q", MAX_SIZE + 1)),
    "nf-inner": ("netfulfilled.dat", NF_MAGIC, cs(1) + SERVICE + cs(MAX_SIZE + 1)),
    "nf-name": ("netfulfilled.dat", NF_MAGIC, cs(1) + SERVICE + cs(1) + cs(MAX_SIZE + 1)),
    # The signature's `LIMITED_VECTOR(vchSig, 65)`: 65 bytes is in bounds (the read stops at
    # `mapSporksActive`), 66 is over the limit, MAX_SIZE + 1 fails `ReadCompactSize` first.
    "sp-sig-65": ("sporks.dat", SP_MAGIC, SPORK_HEAD + cs(65) + bytes(65)),
    "sp-sig-66": ("sporks.dat", SP_MAGIC, SPORK_HEAD + cs(66) + bytes(66)),
    "sp-sig-over": ("sporks.dat", SP_MAGIC, SPORK_HEAD + cs(MAX_SIZE + 1)),
    "mn-version": ("mncache.dat", MN_MAGIC, cs(MAX_SIZE + 1)),
    "mn-infos": ("mncache.dat", MN_MAGIC, s(META_VERSION) + cs(MAX_SIZE + 1)),
    "mn-votes": ("mncache.dat", MN_MAGIC, s(META_VERSION) + cs(1) + INFO_HEAD + cs(MAX_SIZE + 1)),
    "mn-used": ("mncache.dat", MN_MAGIC, s(META_VERSION) + cs(0) + struct.pack("<q", 3)
                + b"\xff" + struct.pack("<Q", 1 << 32)),
    "mn-used-max": ("mncache.dat", MN_MAGIC, s(META_VERSION) + cs(0) + struct.pack("<q", 3)
                    + cs(MAX_SIZE)),
    "mn-used-noncanonical": ("mncache.dat", MN_MAGIC, s(META_VERSION) + cs(0)
                             + struct.pack("<q", 3) + b"\xfe\xff\xff\x00\x00"),
}

LINE = re.compile(r"^\S+ \[error\] (CoreRead: .*)$")


def run(dashd, name, files, port):
    with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR")) as dd:
        os.makedirs(os.path.join(dd, "regtest"), mode=0o700)
        for file, data in files.items():
            with open(os.path.join(dd, "regtest", file), "wb") as f:
                f.write(data)
        p = subprocess.Popen([dashd, "-regtest", f"-datadir={dd}", f"-port={port}",
                              f"-rpcport={port + 1}", "-listen=0", "-printtoconsole=0",
                              "-rpcuser=sp1", "-rpcpassword=sp1"])
        log = os.path.join(dd, "regtest", "debug.log")
        for _ in range(240):
            time.sleep(0.5)
            text = open(log, errors="replace").read() if os.path.exists(log) else ""
            if "init message: Done loading" in text or p.poll() is not None:
                break
        else:
            sys.exit(f"{name}: dashd did not finish loading")
        p.terminate()
        p.wait(120)
        lines = open(log, errors="replace").read().splitlines()
    # dashd verifies netfulfilled.dat, then mncache.dat; each read's line comes before the
    # `CFlatDB::Read` line that names its file. A file Core refuses stops dashd at once.
    got = {}
    for i, line in enumerate(lines):
        m = LINE.match(line)
        if m and ("Magic is ok but data has invalid format" in lines[i + 1]
                  or "File format is unknown or invalid" in lines[i + 1]):
            file = lines[i + 1].split("Error reading ")[1].split(":")[0]
            got.setdefault(file, m.group(1))
    return got


def main():
    dashd = os.environ["P2A_DASHD"]
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    version = subprocess.run([dashd, "-version"], capture_output=True, text=True).stdout.splitlines()[0]
    rows = []
    for i, (name, case) in enumerate(CASES.items()):
        file, magic, body, *raw = case
        data = body if magic is None else flat(magic, body, raw[0] if raw else None)
        with open(os.path.join(out, f"{name}.dat"), "wb") as f:
            f.write(data)
        got = run(dashd, name, {file: data}, 21900 + 2 * i)
        rows.append(f"{name}\t{file}\t{got[file]}")
    with open(os.path.join(out, "coreread-clear.txt"), "w") as f:
        f.write(f"# {version}; contrib/node/sp1-transcripts/clear_vectors.py\n")
        f.write("# case\tfile\tdashd's CoreRead line (debug.log, after the time and [error])\n")
        f.write("\n".join(rows) + "\n")


if __name__ == "__main__":
    main()
