// Records Core's MuHash3072 arithmetic on boundary values, for dash-consensus's
// `muhash_arithmetic_reproduces_cores_on_boundary_values` test (CS5b). Built by muhash-vectors.sh
// against a Dash Core tree at v24.0.0-rc.3 and linked with that tree's crypto libraries and
// memory_cleanse, so every number comes from Core's own crypto/muhash.cpp.
//
// The values below are raw 3072-bit numerators/denominators (Core's serialization: 48
// little-endian 64-bit limbs).
//   products, for each ordered pair (a, b): MuHash3072{a, 1} *= MuHash3072{b, 1}, its numerator
//     (Num3072::Multiply);
//   quotients, for a = 0x55.. and every b, and for b = 2 and the a at or above p − 1:
//     MuHash3072{a, b}.Finalize(), the hash, and the numerator after it (a / b: Num3072::Divide
//     and GetInverse). Each inverse costs about a second in an unoptimised test, hence few.
// Both denominators are then 1; the generator checks it rather than recording it.
// Output: JSON on stdout.

#include <crypto/muhash.h>
#include <streams.h>
#include <uint256.h>

#include <algorithm>
#include <cstdio>
#include <string>
#include <vector>

namespace {

using Bytes = std::vector<unsigned char>;
constexpr size_t N = Num3072::BYTE_SIZE;

Bytes Value(const std::string& name)
{
    Bytes v(N, 0);
    auto set_u64 = [&](size_t limb, uint64_t x) {
        for (int i = 0; i < 8; ++i) v[limb * 8 + i] = (x >> (8 * i)) & 0xff;
    };
    // p = 2^3072 - 1103717.
    auto p_minus = [&](uint64_t k) {
        for (size_t i = 0; i < 48; ++i) set_u64(i, ~uint64_t{0});
        set_u64(0, ~uint64_t{0} - 1103717 + 1 - k);
    };
    auto p_plus = [&](uint64_t k) {
        for (size_t i = 0; i < 48; ++i) set_u64(i, ~uint64_t{0});
        set_u64(0, ~uint64_t{0} - 1103717 + 1 + k);
    };
    if (name == "0") return v;
    if (name == "1") { v[0] = 1; return v; }
    if (name == "2") { v[0] = 2; return v; }
    if (name == "p-2") { p_minus(2); return v; }
    if (name == "p-1") { p_minus(1); return v; }
    if (name == "p") { p_minus(0); return v; }
    if (name == "p+1") { p_plus(1); return v; }
    if (name == "2^3072-1") { for (auto& b : v) b = 0xff; return v; }
    if (name == "2^3071") { v[N - 1] = 0x80; return v; }
    if (name == "2^3008-1") { for (size_t i = 0; i < N - 8; ++i) v[i] = 0xff; return v; }
    if (name == "0x55..") { for (auto& b : v) b = 0x55; return v; }
    if (name == "0xaa..") { for (auto& b : v) b = 0xaa; return v; }
    std::fprintf(stderr, "unknown value %s\n", name.c_str());
    std::abort();
}

MuHash3072 State(const Bytes& num, const Bytes& den)
{
    DataStream s{};
    s.write(MakeByteSpan(num));
    s.write(MakeByteSpan(den));
    MuHash3072 m;
    s >> m;
    return m;
}

std::string Hex(Span<const unsigned char> bytes)
{
    static const char* digits = "0123456789abcdef";
    std::string out;
    for (unsigned char b : bytes) {
        out += digits[b >> 4];
        out += digits[b & 15];
    }
    return out;
}

/// The numerator, after checking that the denominator is 1.
std::string Numerator(const MuHash3072& m)
{
    DataStream s{};
    s << m;
    const Span<const unsigned char> bytes = MakeUCharSpan(s);
    const Bytes one = Value("1");
    if (!std::equal(one.begin(), one.end(), bytes.begin() + N)) {
        std::fprintf(stderr, "a denominator is not 1\n");
        std::abort();
    }
    return Hex(bytes.first(N));
}

} // namespace

int main()
{
    const std::vector<std::string> names{"0", "1", "2", "p-2", "p-1", "p", "p+1", "2^3072-1",
                                         "2^3071", "2^3008-1", "0x55..", "0xaa.."};
    const Bytes one = Value("1");
    std::printf("{\n \"values\": {\n");
    for (size_t i = 0; i < names.size(); ++i) {
        std::printf("  \"%s\": \"%s\"%s\n", names[i].c_str(), Hex(Value(names[i])).c_str(),
                    i + 1 < names.size() ? "," : "");
    }
    std::printf(" },\n \"products\": [\n");
    for (size_t i = 0; i < names.size(); ++i) {
        for (size_t j = 0; j < names.size(); ++j) {
            MuHash3072 mul = State(Value(names[i]), one);
            mul *= State(Value(names[j]), one);
            const bool last = i + 1 == names.size() && j + 1 == names.size();
            std::printf("  {\"a\": \"%s\", \"b\": \"%s\", \"numerator\": \"%s\"}%s\n",
                        names[i].c_str(), names[j].c_str(), Numerator(mul).c_str(), last ? "" : ",");
        }
    }
    std::vector<std::pair<std::string, std::string>> quotients;
    for (const auto& b : names) quotients.emplace_back("0x55..", b);
    for (const auto& a : {"p-1", "p", "p+1", "2^3072-1"}) quotients.emplace_back(a, "2");
    std::printf(" ],\n \"quotients\": [\n");
    for (size_t i = 0; i < quotients.size(); ++i) {
        const auto& [a, b] = quotients[i];
        MuHash3072 fin = State(Value(a), Value(b));
        uint256 hash;
        fin.Finalize(hash);
        std::printf("  {\"a\": \"%s\", \"b\": \"%s\", \"hash\": \"%s\", \"numerator\": \"%s\"}%s\n",
                    a.c_str(), b.c_str(), Hex(Span{hash.begin(), hash.size()}).c_str(),
                    Numerator(fin).c_str(), i + 1 < quotients.size() ? "," : "");
    }
    std::printf(" ]\n}\n");
}
