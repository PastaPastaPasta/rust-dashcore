#!/usr/bin/env python3
"""Record operator signatures using unmodified Core v24.0.0 C++ signing code."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[3]
COMMIT = "938fba502824b9d850b7c710601456dc3d71603e"
HASHES = [bytes(range(32)).hex(), (bytes([0x42]) * 32).hex()]


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core", required=True, type=Path)
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--fixture", type=Path, default=ROOT / "dashd-masternode/tests/fixtures/am1-key/core-v24.json")
    args = parser.parse_args()
    commit = subprocess.check_output(["git", "-C", args.core, "rev-parse", "v24.0.0^{commit}"], text=True).strip()
    if commit != COMMIT:
        raise RuntimeError(f"wrong Core commit: {commit}")
    args.work.mkdir(parents=True, exist_ok=True)
    source = args.work.resolve() / "core"
    if not source.exists():
        source.mkdir()
        archive = subprocess.Popen(["git", "-C", args.core, "archive", COMMIT], stdout=subprocess.PIPE)
        with tarfile.open(fileobj=archive.stdout, mode="r|*") as tree:
            tree.extractall(source, filter="data")
        if archive.wait() != 0:
            raise RuntimeError("Core source export failed")
    bls = source / "src/dashbls"
    subprocess.run(["./autogen.sh"], cwd=bls, check=True)
    subprocess.run(["./configure", "--disable-tests", "--disable-bench", "--disable-shared"], cwd=bls, check=True)
    subprocess.run(["nice", "-n", "10", "make", "-j12"], cwd=bls, check=True)
    helper = args.work.resolve() / "signatures"
    command = ["g++", "-std=c++20", "-O2", "-ffunction-sections", "-fdata-sections", "-DBUILD_BITCOIN_INTERNAL",
               f"-I{source / 'src'}", f"-I{bls / 'include'}", f"-I{bls / 'depends/relic/include'}",
               f"-I{bls / 'depends/mimalloc/include'}", str(Path(__file__).with_suffix(".cpp")),
               str(source / "src/bls/bls.cpp"), str(bls / ".libs/libdashbls.a"),
               "-Wl,--gc-sections", "-lgmp", "-lpthread", "-o", str(helper)]
    subprocess.run(command, check=True)
    fixture = json.loads(args.fixture.read_text())
    rows = []
    for key in fixture["network_keys"][:2]:
        for raw_hash in HASHES:
            output = subprocess.check_output([helper, key["secret"], raw_hash], text=True)
            signatures = dict(line.split("=", 1) for line in output.splitlines())
            if set(signatures) != {"legacy", "basic", "sign_basic"}:
                raise RuntimeError("missing signature variant")
            if any(len(bytes.fromhex(sig)) != 96 for sig in signatures.values()):
                raise RuntimeError("wrong signature size")
            rows.append({"network_index": key["index"], "hash_bytes": raw_hash, **signatures})
    fixture["signatures"] = {
        "core_commit": commit,
        "helper_source_sha256": sha256(Path(__file__).with_suffix(".cpp")),
        "helper_binary_sha256": sha256(helper),
        "dashbls_archive_sha256": sha256(bls / ".libs/libdashbls.a"),
        "compiler": subprocess.check_output(["g++", "--version"], text=True).splitlines()[0],
        "rows": rows,
    }
    args.fixture.write_text(json.dumps(fixture, indent=2, ensure_ascii=True) + "\n")
    print(f"recorded {len(rows)} rows, three variants each, in {args.fixture}")


if __name__ == "__main__":
    main()
