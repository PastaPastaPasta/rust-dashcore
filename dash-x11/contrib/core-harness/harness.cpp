// Runs Dash Core's X11 (src/hash_x11.h over src/crypto/x11, compiled unmodified by build.sh)
// for dash-x11's differential tests and instruction counts.
//
// Usage:
//   harness hash <hex>                  HashX11 of <hex> ("-" for empty), as hex on one line
//   harness stage-hex <name> <hex>      one chain function of <hex>, as hex on one line
//   harness chain <hex>                 the 11 intermediate 64-byte digests, one hex line each
//   harness stream <kind> <seed> <n>    HashX11 of n generated inputs, raw 32-byte digests on
//                                       stdout (see Generate below; dash-x11's differential
//                                       generates the same inputs)
//   harness stage <name> <seed> <n>    one function of the chain (blake, bmw, ..., echo) over n
//                                       generated inputs, raw 64-byte digests on stdout: blake
//                                       gets the "random" kind's inputs, the others 64 bytes
//   harness headers                     HashX11 of each 80-byte record on stdin, raw 32-byte
//                                       digests on stdout
//   harness bench <stage> <n>           run <stage> n times for perf stat (x11 on an 80-byte
//                                       zero buffer, as bench_dash's Pow_X11_0080b; a single
//                                       function on a 64-byte zero buffer, blake on 80 bytes,
//                                       as inside HashX11)
//   harness name                        the dispatch Core selected
// SapphireAutoDetect() runs first, as Core's kernel context and bench_dash do. With the
// environment variable DASH_X11_CORE_SOFT set, the harness then puts back Core's software ECHO
// and SHAvite-3 functions, so they can serve as the reference too.

#include <crypto/x11/dispatch.h>
#include <hash_x11.h>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <stdexcept>
#include <string>
#include <vector>

extern sapphire::dispatch::EchoShiftMix echo_shift_mix;
extern sapphire::dispatch::EchoRoundFn echo_round;
extern sapphire::dispatch::ShaviteCompressFn shavite_c512;
namespace sapphire {
namespace soft_echo {
void FullStateRound(uint64_t W[16][2], uint32_t& k0, uint32_t& k1, uint32_t& k2, uint32_t& k3);
void ShiftAndMix(uint64_t W[16][2]);
} // namespace soft_echo
namespace soft_shavite {
void Compress(sph_shavite_big_context* sc, const void* msg);
} // namespace soft_shavite
} // namespace sapphire

namespace {

std::vector<unsigned char> FromHex(const std::string& hex)
{
    if (hex == "-") return {};
    if (hex.size() % 2) throw std::runtime_error("odd hex");
    std::vector<unsigned char> out(hex.size() / 2);
    for (size_t i = 0; i < out.size(); ++i) out[i] = std::stoi(hex.substr(2 * i, 2), nullptr, 16);
    return out;
}

void PrintHex(const unsigned char* data, size_t len)
{
    for (size_t i = 0; i < len; ++i) std::printf("%02x", data[i]);
    std::printf("\n");
}

uint256 X11(const std::vector<unsigned char>& in) { return HashX11(in.begin(), in.end()); }

// SplitMix64, the generator dash-x11's differential test also implements.
struct SplitMix64 {
    uint64_t state;
    uint64_t Next()
    {
        uint64_t z = (state += 0x9e3779b97f4a7c15ULL);
        z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ULL;
        z = (z ^ (z >> 27)) * 0x94d049bb133111ebULL;
        return z ^ (z >> 31);
    }
};

// Case i of a stream: "random" has length i % 201 (every length 0..=200), "long" has length
// 201 + i % 900 (201..=1100, BLAKE's multi-block paths), "header" is 80 bytes, "64" is 64 bytes
// (a chain function's input).
// The bytes are the generator's next words, least significant byte first; a partial last word
// uses its low bytes.
void Generate(SplitMix64& rng, const std::string& kind, uint64_t i, std::vector<unsigned char>& out)
{
    size_t len;
    if (kind == "random") {
        len = i % 201;
    } else if (kind == "long") {
        len = 201 + i % 900;
    } else if (kind == "header") {
        len = 80;
    } else if (kind == "64") {
        len = 64;
    } else {
        throw std::runtime_error("unknown stream kind");
    }
    out.resize(len);
    for (size_t j = 0; j < len; j += 8) {
        uint64_t w = rng.Next();
        for (size_t k = 0; k < 8 && j + k < len; ++k) out[j + k] = static_cast<unsigned char>(w >> (8 * k));
    }
}

void Chain(const std::vector<unsigned char>& in)
{
    sph_blake512_context ctx_blake;
    sph_bmw512_context ctx_bmw;
    sph_groestl512_context ctx_groestl;
    sph_jh512_context ctx_jh;
    sph_keccak512_context ctx_keccak;
    sph_skein512_context ctx_skein;
    sph_luffa512_context ctx_luffa;
    sph_cubehash512_context ctx_cubehash;
    sph_shavite512_context ctx_shavite;
    sph_simd512_context ctx_simd;
    sph_echo512_context ctx_echo;
    static const unsigned char pblank[1]{};
    unsigned char h[11][64];

    sph_blake512_init(&ctx_blake);
    sph_blake512(&ctx_blake, in.empty() ? pblank : in.data(), in.size());
    sph_blake512_close(&ctx_blake, h[0]);
    sph_bmw512_init(&ctx_bmw);
    sph_bmw512(&ctx_bmw, h[0], 64);
    sph_bmw512_close(&ctx_bmw, h[1]);
    sph_groestl512_init(&ctx_groestl);
    sph_groestl512(&ctx_groestl, h[1], 64);
    sph_groestl512_close(&ctx_groestl, h[2]);
    sph_skein512_init(&ctx_skein);
    sph_skein512(&ctx_skein, h[2], 64);
    sph_skein512_close(&ctx_skein, h[3]);
    sph_jh512_init(&ctx_jh);
    sph_jh512(&ctx_jh, h[3], 64);
    sph_jh512_close(&ctx_jh, h[4]);
    sph_keccak512_init(&ctx_keccak);
    sph_keccak512(&ctx_keccak, h[4], 64);
    sph_keccak512_close(&ctx_keccak, h[5]);
    sph_luffa512_init(&ctx_luffa);
    sph_luffa512(&ctx_luffa, h[5], 64);
    sph_luffa512_close(&ctx_luffa, h[6]);
    sph_cubehash512_init(&ctx_cubehash);
    sph_cubehash512(&ctx_cubehash, h[6], 64);
    sph_cubehash512_close(&ctx_cubehash, h[7]);
    sph_shavite512_init(&ctx_shavite);
    sph_shavite512(&ctx_shavite, h[7], 64);
    sph_shavite512_close(&ctx_shavite, h[8]);
    sph_simd512_init(&ctx_simd);
    sph_simd512(&ctx_simd, h[8], 64);
    sph_simd512_close(&ctx_simd, h[9]);
    sph_echo512_init(&ctx_echo);
    sph_echo512(&ctx_echo, h[9], 64);
    sph_echo512_close(&ctx_echo, h[10]);
    for (auto& d : h) PrintHex(d, 64);
}

// One function of the chain over one input, as HashX11 calls it.
#define ONE(name)                                               \
    if (stage == #name) {                                       \
        sph_##name##512_context ctx;                            \
        sph_##name##512_init(&ctx);                             \
        sph_##name##512(&ctx, in.empty() ? pblank : in.data(), in.size()); \
        sph_##name##512_close(&ctx, out);                       \
        return;                                                 \
    }

void Stage(const std::string& stage, const std::vector<unsigned char>& in, unsigned char out[64])
{
    static const unsigned char pblank[1]{};
    ONE(blake) ONE(bmw) ONE(groestl) ONE(skein) ONE(jh) ONE(keccak) ONE(luffa) ONE(cubehash)
    ONE(shavite) ONE(simd) ONE(echo)
    throw std::runtime_error("unknown stage");
}

// One function of the chain: init, one write, close, into a 64-byte buffer.
#define RUN(name, len)                                          \
    do {                                                        \
        sph_##name##512_context ctx;                            \
        unsigned char out[64];                                  \
        for (uint64_t i = 0; i < n; ++i) {                      \
            sph_##name##512_init(&ctx);                         \
            sph_##name##512(&ctx, buf, len);                    \
            sph_##name##512_close(&ctx, out);                   \
            asm volatile("" : : "r"(out) : "memory");           \
        }                                                       \
    } while (0)

int Bench(const std::string& stage, uint64_t n)
{
    unsigned char buf[80]{};
    if (stage == "x11") {
        std::vector<unsigned char> in(80, 0);
        uint256 hash;
        for (uint64_t i = 0; i < n; ++i) {
            hash = X11(in);
            asm volatile("" : : "r"(&hash) : "memory");
        }
    } else if (stage == "blake") {
        RUN(blake, 80);
    } else if (stage == "bmw") {
        RUN(bmw, 64);
    } else if (stage == "groestl") {
        RUN(groestl, 64);
    } else if (stage == "skein") {
        RUN(skein, 64);
    } else if (stage == "jh") {
        RUN(jh, 64);
    } else if (stage == "keccak") {
        RUN(keccak, 64);
    } else if (stage == "luffa") {
        RUN(luffa, 64);
    } else if (stage == "cubehash") {
        RUN(cubehash, 64);
    } else if (stage == "shavite") {
        RUN(shavite, 64);
    } else if (stage == "simd") {
        RUN(simd, 64);
    } else if (stage == "echo") {
        RUN(echo, 64);
    } else {
        std::fprintf(stderr, "unknown stage %s\n", stage.c_str());
        return 2;
    }
    return 0;
}

} // namespace

int main(int argc, char** argv)
{
    SapphireAutoDetect();
    if (std::getenv("DASH_X11_CORE_SOFT")) {
        echo_round = sapphire::soft_echo::FullStateRound;
        echo_shift_mix = sapphire::soft_echo::ShiftAndMix;
        shavite_c512 = sapphire::soft_shavite::Compress;
    }
    if (argc < 2) {
        std::fprintf(stderr, "usage: harness hash|stage-hex|chain|stream|stage|headers|bench|name ...\n");
        return 2;
    }
    const std::string cmd = argv[1];
    if (cmd == "hash" && argc == 3) {
        const uint256 h = X11(FromHex(argv[2]));
        PrintHex(h.begin(), 32);
    } else if (cmd == "stage-hex" && argc == 4) {
        unsigned char d[64];
        Stage(argv[2], FromHex(argv[3]), d);
        PrintHex(d, 64);
    } else if (cmd == "chain" && argc == 3) {
        Chain(FromHex(argv[2]));
    } else if (cmd == "stream" && argc == 5) {
        const std::string kind = argv[2];
        SplitMix64 rng{std::strtoull(argv[3], nullptr, 0)};
        const uint64_t n = std::strtoull(argv[4], nullptr, 0);
        std::vector<unsigned char> in;
        std::vector<unsigned char> out;
        out.reserve(32 * 4096);
        for (uint64_t i = 0; i < n; ++i) {
            Generate(rng, kind, i, in);
            const uint256 h = X11(in);
            out.insert(out.end(), h.begin(), h.end());
            if (out.size() >= 32 * 4096 || i + 1 == n) {
                if (std::fwrite(out.data(), 1, out.size(), stdout) != out.size()) return 1;
                out.clear();
            }
        }
        std::fflush(stdout);
    } else if (cmd == "stage" && argc == 5) {
        const std::string stage = argv[2];
        SplitMix64 rng{std::strtoull(argv[3], nullptr, 0)};
        const uint64_t n = std::strtoull(argv[4], nullptr, 0);
        std::vector<unsigned char> in;
        std::vector<unsigned char> out;
        out.reserve(64 * 4096);
        for (uint64_t i = 0; i < n; ++i) {
            if (stage == "blake") {
                Generate(rng, "random", i, in);
            } else {
                Generate(rng, "64", i, in);
            }
            unsigned char d[64];
            Stage(stage, in, d);
            out.insert(out.end(), d, d + 64);
            if (out.size() >= 64 * 4096 || i + 1 == n) {
                if (std::fwrite(out.data(), 1, out.size(), stdout) != out.size()) return 1;
                out.clear();
            }
        }
        std::fflush(stdout);
    } else if (cmd == "headers" && argc == 2) {
        std::vector<unsigned char> in(80);
        while (std::fread(in.data(), 1, 80, stdin) == 80) {
            const uint256 h = X11(in);
            if (std::fwrite(h.begin(), 1, 32, stdout) != 32) return 1;
        }
        std::fflush(stdout);
    } else if (cmd == "bench" && argc == 4) {
        return Bench(argv[2], std::strtoull(argv[3], nullptr, 0));
    } else if (cmd == "name" && argc == 2) {
        std::printf("echo_round=%s echo_shift_mix=%s shavite=%s\n",
                    echo_round == sapphire::soft_echo::FullStateRound ? "soft" : "simd",
                    echo_shift_mix == sapphire::soft_echo::ShiftAndMix ? "soft" : "simd",
                    shavite_c512 == sapphire::soft_shavite::Compress ? "soft" : "simd");
    } else {
        std::fprintf(stderr, "bad command\n");
        return 2;
    }
    return 0;
}
