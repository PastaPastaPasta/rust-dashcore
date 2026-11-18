// Copyright (c) 2026 The Dash Core developers
// Distributed under the MIT software license, see the accompanying
// file COPYING or http://www.opensource.org/licenses/mit-license.php.

#include <bls/bls.h>

#include <algorithm>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

static std::vector<uint8_t> ReadHex(const std::string& text)
{
    if (text.size() != 64) throw std::invalid_argument("expected 32 bytes");
    std::vector<uint8_t> bytes;
    for (size_t i = 0; i < text.size(); i += 2) {
        bytes.push_back(std::stoul(text.substr(i, 2), nullptr, 16));
    }
    return bytes;
}

static void Print(const char* name, const std::vector<uint8_t>& bytes)
{
    const char* digits = "0123456789abcdef";
    std::cout << name << '=';
    for (auto byte : bytes) std::cout << digits[byte >> 4] << digits[byte & 15];
    std::cout << '\n';
}

int main(int argc, char** argv)
{
    if (argc != 3) return 2;
    CBLSSecretKey key;
    key.SetBytes(ReadHex(argv[1]), false);
    if (!key.IsValid()) return 3;
    const auto raw_hash = ReadHex(argv[2]);
    uint256 hash;
    std::copy(raw_hash.begin(), raw_hash.end(), hash.begin());
    // Core: active/masternode.cpp:290-294
    const auto legacy = key.Sign(hash, true);
    const auto basic = key.Sign(hash, false);
    if (!legacy.IsValid() || !basic.IsValid()) return 4;
    Print("legacy", legacy.ToByteVector(true));
    Print("basic", basic.ToByteVector(false));
    // Core: active/masternode.cpp:297-303
    const auto sign_basic = key.Sign(hash, false);
    if (!sign_basic.IsValid()) return 5;
    Print("sign_basic", sign_basic.ToByteVector(false));
}
