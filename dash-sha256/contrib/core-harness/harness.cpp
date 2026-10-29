// Runs Dash Core's SHA-256 (src/crypto/sha256*.cpp, compiled unmodified next to this file by
// build.sh) for dash-sha256's differential tests.
//
// Commands, one per line on stdin, one answer line each on stdout:
//   name <use>                     SHA256AutoDetect(<use>)'s name
//   sha256 <use> <splits> <hex>    CSHA256 of <hex>, written in pieces of the comma-separated
//                                  lengths in <splits> ("-" for one write; the last piece is the rest)
//   d64 <use> <hex>                SHA256D64 of <hex>, a multiple of 64 bytes
//   fixture                        the fixture JSON (see Fixture below), checked across every <use>
//   long-fixture                   the long-message fixture JSON (see LongFixture below)
// <use> is sha256_implementation::UseImplementation as a number (0 standard ... 7 all).

#include <crypto/sha256.h>

#include <algorithm>
#include <cstdint>
#include <iterator>
#include <cstdio>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {

std::string g_impl_name;
int g_impl_use = -1;

void Select(int use)
{
    if (use != g_impl_use) {
        g_impl_name = SHA256AutoDetect(static_cast<sha256_implementation::UseImplementation>(use));
        g_impl_use = use;
    }
}

std::vector<unsigned char> FromHex(const std::string& hex)
{
    if (hex == "-") return {};
    if (hex.size() % 2) throw std::runtime_error("odd hex");
    std::vector<unsigned char> out(hex.size() / 2);
    for (size_t i = 0; i < out.size(); ++i) out[i] = std::stoi(hex.substr(2 * i, 2), nullptr, 16);
    return out;
}

std::string ToHex(const unsigned char* data, size_t len)
{
    static const char* digits = "0123456789abcdef";
    std::string out;
    for (size_t i = 0; i < len; ++i) {
        out += digits[data[i] >> 4];
        out += digits[data[i] & 15];
    }
    return out;
}

std::string Sha256(const std::vector<unsigned char>& data, const std::vector<size_t>& splits)
{
    CSHA256 hasher;
    size_t pos = 0;
    for (size_t len : splits) {
        if (len > data.size() - pos) throw std::runtime_error("split past the end");
        hasher.Write(data.data() + pos, len);
        pos += len;
    }
    hasher.Write(data.data() + pos, data.size() - pos);
    unsigned char out[CSHA256::OUTPUT_SIZE];
    hasher.Finalize(out);
    return ToHex(out, sizeof(out));
}

std::string D64(const std::vector<unsigned char>& data)
{
    if (data.size() % 64) throw std::runtime_error("d64 input not a multiple of 64");
    std::vector<unsigned char> out(data.size() / 2);
    SHA256D64(out.data(), data.data(), data.size() / 64);
    return ToHex(out.data(), out.size());
}

// splitmix64, mirrored by dash-sha256's tests/core.rs.
struct Rng {
    uint64_t s;
    uint64_t Next()
    {
        uint64_t z = (s += 0x9e3779b97f4a7c15ULL);
        z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ULL;
        z = (z ^ (z >> 27)) * 0x94d049bb133111ebULL;
        return z ^ (z >> 31);
    }
    std::vector<unsigned char> Bytes(size_t n)
    {
        std::vector<unsigned char> out(n);
        for (auto& b : out) b = Next() >> 56;
        return out;
    }
};

// Write sizes in the style of Core's crypto_tests TestVector: random, often small, may be zero.
std::vector<size_t> Splits(Rng& rng, size_t len)
{
    std::vector<size_t> out;
    size_t pos = 0;
    while (pos < len) {
        size_t piece = rng.Next() % ((len - pos + 1) / 2 + 1);
        out.push_back(piece);
        pos += piece;
    }
    return out;
}

const int USES[] = {0, 1, 3, 5, 7}; // STANDARD, SSE4, SSE4|AVX2, SSE4|SHANI, ALL

// SHA-256 of inputs of every length 0..320 and some longer ones, whole and in pieces; SHA256D64
// of 0..33 inputs. The inputs come from Rng, seeded by the case. Every implementation this CPU
// has must agree, or the fixture is not written.
void Fixture()
{
    std::vector<size_t> lens;
    for (size_t n = 0; n <= 320; ++n) lens.push_back(n);
    for (size_t n : {447, 448, 449, 511, 512, 513, 1023, 1024, 1025, 4095, 4096, 4097, 10000, 65536, 100003}) lens.push_back(n);

    std::ostringstream json;
    json << "{\n  \"sha256\": [\n";
    for (size_t c = 0; c < lens.size(); ++c) {
        Rng rng{lens[c]};
        auto data = rng.Bytes(lens[c]);
        auto splits = Splits(rng, lens[c]);
        std::string digest;
        for (int use : USES) {
            Select(use);
            for (const auto& d : {Sha256(data, {}), Sha256(data, splits)}) {
                if (digest.empty()) digest = d;
                if (d != digest) throw std::runtime_error("implementations disagree on sha256 len " + std::to_string(lens[c]));
            }
        }
        json << "    {\"len\": " << lens[c] << ", \"digest\": \"" << digest << "\"}" << (c + 1 < lens.size() ? "," : "") << "\n";
    }
    json << "  ],\n  \"sha256d64\": [\n";
    for (size_t n = 0; n <= 33; ++n) {
        Rng rng{1000000 + n};
        auto data = rng.Bytes(64 * n);
        std::string digests;
        for (int use : USES) {
            Select(use);
            auto d = D64(data);
            if (digests.empty() && n) digests = d;
            if (d != digests) throw std::runtime_error("implementations disagree on d64 n " + std::to_string(n));
        }
        json << "    {\"n\": " << n << ", \"digests\": \"" << digests << "\"}" << (n < 33 ? "," : "") << "\n";
    }
    json << "  ]\n}\n";
    std::cout << json.str();
}

// SHA-256 of messages around 2^29 bytes, where the bit length passes 2^32: a 1 MiB block from Rng
// seeded with 2^29, repeated, cut at each length. The lengths end at 63, 0, 1, 55, 56, 63 and 0
// mod 64, the padding cases. Every implementation must agree, as in Fixture.
void LongFixture()
{
    const uint64_t seed = 1ULL << 29;
    const size_t pattern_len = 1 << 20;
    const uint64_t lens[] = {536870911, 536870912, 536870913, 536870967, 536870968, 536870975, 536870976};
    Rng rng{seed};
    const auto pattern = rng.Bytes(pattern_len);

    std::vector<std::string> digests;
    for (int use : USES) {
        Select(use);
        CSHA256 hasher;
        uint64_t pos = 0;
        for (size_t c = 0; c < std::size(lens); ++c) {
            while (pos < lens[c]) {
                size_t off = pos % pattern_len;
                size_t len = std::min<uint64_t>(pattern_len - off, lens[c] - pos);
                hasher.Write(pattern.data() + off, len);
                pos += len;
            }
            unsigned char out[CSHA256::OUTPUT_SIZE];
            CSHA256(hasher).Finalize(out);
            std::string d = ToHex(out, sizeof(out));
            if (digests.size() <= c) digests.push_back(d);
            if (d != digests[c]) throw std::runtime_error("implementations disagree on long len " + std::to_string(lens[c]));
        }
    }

    std::cout << "{\n  \"seed\": " << seed << ",\n  \"pattern_len\": " << pattern_len << ",\n  \"sha256\": [\n";
    for (size_t c = 0; c < std::size(lens); ++c) {
        std::cout << "    {\"len\": " << lens[c] << ", \"digest\": \"" << digests[c] << "\"}" << (c + 1 < std::size(lens) ? "," : "") << "\n";
    }
    std::cout << "  ]\n}\n";
}

} // namespace

int main()
{
    std::string line;
    while (std::getline(std::cin, line)) {
        std::istringstream in(line);
        std::string cmd;
        in >> cmd;
        try {
            if (cmd == "name") {
                int use;
                in >> use;
                Select(use);
                std::cout << g_impl_name << "\n";
            } else if (cmd == "sha256") {
                int use;
                std::string splits, hex;
                in >> use >> splits >> hex;
                std::vector<size_t> lens;
                if (splits != "-") {
                    std::istringstream s(splits);
                    for (std::string piece; std::getline(s, piece, ',');) lens.push_back(std::stoull(piece));
                }
                Select(use);
                std::cout << Sha256(FromHex(hex), lens) << "\n";
            } else if (cmd == "d64") {
                int use;
                std::string hex;
                in >> use >> hex;
                Select(use);
                std::cout << D64(FromHex(hex)) << "\n";
            } else if (cmd == "fixture") {
                Fixture();
            } else if (cmd == "long-fixture") {
                LongFixture();
            } else {
                throw std::runtime_error("unknown command " + cmd);
            }
        } catch (const std::exception& e) {
            std::cout << "error: " << e.what() << "\n";
        }
        std::cout.flush();
    }
}
