#!/usr/bin/env python3
"""Record Core v24.0.0 net_processing.cpp:3906-3913."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("core", type=Path)
    args = parser.parse_args()
    core = args.core.resolve()
    commit = subprocess.check_output([
        "git", "-C", str(core), "rev-parse", "HEAD",
    ], text=True).strip()
    if commit != "938fba502824b9d850b7c710601456dc3d71603e":
        parser.error("Core v24.0.0 commit 938fba502824b9d850b7c710601456dc3d71603e required")
    subprocess.run(["git", "-C", str(core), "diff", "--exit-code", "HEAD"], check=True)
    src = core / "src"
    processing = (src / "net_processing.cpp").read_text()
    start = processing.index('    const std::string PLATFORM_BAN_REQUESTID_PREFIX =')
    end = processing.index('\n    if (!ban_msg.m_signature.VerifyInsecure', start)
    expressions = processing[start:end]
    verify = processing[end:].splitlines()[1].strip()
    verify = verify.removeprefix("if (!").removesuffix(") {")
    template = Path(__file__).with_name("record.cpp").read_text()
    source = template.replace(
        "// Core: v24.0.0 net_processing.cpp:3906-3911.", expressions,
    ).replace("CORE_VERIFY", verify)
    with tempfile.TemporaryDirectory(prefix="am1-platformban-") as temporary:
        tmp = Path(temporary)
        bls_src = tmp / "dashbls"
        shutil.copytree(src / "dashbls", bls_src)
        subprocess.run(["./autogen.sh"], cwd=bls_src, check=True, stdout=subprocess.DEVNULL)
        subprocess.run([
            "./configure", "--disable-tests", "--disable-bench", "--with-backend=gmp",
        ], cwd=bls_src, check=True, stdout=subprocess.DEVNULL)
        subprocess.run([
            "make", "-j12", "libdashbls.la",
        ], cwd=bls_src, check=True, stdout=subprocess.DEVNULL)
        cpp = tmp / "record.cpp"
        binary = tmp / "record"
        cpp.write_text(source)
        subprocess.run([
            os.environ.get("CXX", "c++"), "-std=c++20", "-O2",
            "-DDISABLE_OPTIMIZED_SHA256", "-DBUILD_BITCOIN_INTERNAL",
            "-ffunction-sections", "-fdata-sections", "-Wl,--gc-sections",
            f"-I{src}", f"-I{bls_src / 'include'}",
            f"-I{bls_src / 'depends/relic/include'}",
            str(cpp), str(src / "bls/bls.cpp"), str(src / "llmq/signhash.cpp"),
            str(src / "crypto/sha256.cpp"),
            str(bls_src / ".libs/libdashbls.a"),
            "-lgmp", "-lpthread", "-o", str(binary),
        ], check=True)
        output = subprocess.check_output([str(binary)], text=True)
        rows = [json.loads(row) for row in output.splitlines()]
    print(json.dumps({
        "core_commit": commit,
        "core_lines": "net_processing.cpp:3906-3913; llmq/signhash.cpp:14-23; bls/bls.cpp:310-325",
        "byte_order": "stored",
        "vectors": rows,
    }, indent=2))


if __name__ == "__main__":
    main()
