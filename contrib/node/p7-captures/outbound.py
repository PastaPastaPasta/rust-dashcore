#!/usr/bin/env python3
"""Print (input index, input index, winner index) using Core's outbound selector."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("core", type=Path, help="Dash Core v24.0.0 source tree")
    args = parser.parse_args()
    source_dir = args.core.resolve() / "src"
    utils = (source_dir / "llmq/utils.cpp").read_text()
    start = utils.index("uint256 DeterministicOutboundConnection(")
    end = utils.index("\n}", start) + 2
    source = """#include <hash.h>
#include <array>
#include <cstdio>
#include <tuple>
""" + utils[start:end] + """
int main() {
    std::array<std::array<unsigned char, 32>, 6> inputs{};
    inputs[1][0] = 1;
    inputs[2][0] = 2;
    inputs[3][0] = 3;
    inputs[4][0] = 42;
    inputs[5][31] = 1;
    for (size_t i = 0; i < inputs.size(); ++i) {
        for (size_t j = i; j < inputs.size(); ++j) {
            uint256 a{Span<const unsigned char>{inputs[i]}};
            uint256 b{Span<const unsigned char>{inputs[j]}};
            auto result = DeterministicOutboundConnection(a, b);
            std::printf("%zu %zu %zu\\n", i, j, result == a ? i : j);
        }
    }
}
"""
    with tempfile.TemporaryDirectory(prefix="p7-outbound-") as tmp:
        cpp = Path(tmp) / "outbound.cpp"
        binary = Path(tmp) / "outbound"
        cpp.write_text(source)
        subprocess.run([
            os.environ.get("CXX", "c++"), "-std=c++20", "-O2", "-DDISABLE_OPTIMIZED_SHA256",
            f"-I{source_dir}", str(cpp), str(source_dir / "crypto/sha256.cpp"),
            "-o", str(binary),
        ], check=True)
        subprocess.run([str(binary)], check=True)


if __name__ == "__main__":
    main()
