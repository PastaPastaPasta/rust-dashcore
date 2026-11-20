// Records Dash Core's RNGState::MixExtract, AddEvent and SeedEvents over a chain of calls, for
// dashd-random's `mix_extract_and_events_match_cores_vectors` test (BL-502 test 3). Built by
// rng-vectors.sh, which cuts the RNGState class out of v24.0.0-rc.5's src/random.cpp verbatim
// (only `class RNGState {` becomes `struct RNGState {`, so the harness reads m_state and
// m_counter) and links it with that tag's crypto/sha256.cpp, crypto/sha512.cpp and
// support/cleanse.cpp.
//
// The performance counter AddEvent reads is the value each "event" step names. The class's
// mutexes are std::mutex; its thread-safety annotations expand to nothing.
//
// Steps, in order on one RNGState:
//   {"op":"event","info":N,"perf":N}   AddEvent(info), with GetPerformanceCounter() = perf
//   {"op":"mix","events":B,"input":H,"num":N,"strong":B,
//    "ret":B,"out":H,"state":H,"counter":N}
//       CSHA512 h; if events, SeedEvents(h); h.Write(input); ret = MixExtract(out, num, h, strong);
//       then the state's m_state and m_counter.
// Output: JSON on stdout.

#include <crypto/sha256.h>
#include <crypto/sha512.h>
#include <support/cleanse.h>

#include <cassert>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <string>
#include <utility>
#include <vector>

using Mutex = std::mutex;
#define LOCK_CAT2(a, b) a##b
#define LOCK_CAT(a, b) LOCK_CAT2(a, b)
#define LOCK(cs) std::lock_guard<std::mutex> LOCK_CAT(lock_, __LINE__)(cs)
#define GUARDED_BY(x)
#define EXCLUSIVE_LOCKS_REQUIRED(...)

static int64_t g_perf = 0;
static int64_t GetPerformanceCounter() noexcept { return g_perf; }
static void InitHardwareRand() {}

#include "rngstate.inc"

namespace {

using Bytes = std::vector<unsigned char>;

std::string Hex(const unsigned char* p, size_t n)
{
    static const char* digits = "0123456789abcdef";
    std::string s;
    for (size_t i = 0; i < n; ++i) {
        s += digits[p[i] >> 4];
        s += digits[p[i] & 15];
    }
    return s;
}

// xorshift64*, for the inputs only; the vectors record every input they use.
uint64_t g_x = 0x9e3779b97f4a7c15ULL;
uint64_t Next()
{
    g_x ^= g_x >> 12;
    g_x ^= g_x << 25;
    g_x ^= g_x >> 27;
    return g_x * 0x2545f4914f6cdd1dULL;
}

Bytes RandomBytes(size_t n)
{
    Bytes b(n);
    for (auto& c : b) c = Next() >> 56;
    return b;
}

RNGState g_rng;
bool g_first = true;

void Sep()
{
    std::printf("%s\n", g_first ? "" : ",");
    g_first = false;
}

void Event(uint32_t info, int64_t perf)
{
    g_perf = perf;
    g_rng.AddEvent(info);
    Sep();
    std::printf("  {\"op\":\"event\",\"info\":%u,\"perf\":%lld}", info, (long long)perf);
}

void Mix(bool events, size_t input_len, size_t num, bool strong)
{
    Bytes input = RandomBytes(input_len);
    CSHA512 h;
    if (events) g_rng.SeedEvents(h);
    if (!input.empty()) h.Write(input.data(), input.size());
    unsigned char out[32] = {};
    bool ret = g_rng.MixExtract(num ? out : nullptr, num, std::move(h), strong);
    Sep();
    std::printf("  {\"op\":\"mix\",\"events\":%s,\"input\":\"%s\",\"num\":%zu,\"strong\":%s,"
                "\"ret\":%s,\"out\":\"%s\",\"state\":\"%s\",\"counter\":%llu}",
        events ? "true" : "false", Hex(input.data(), input.size()).c_str(), num,
        strong ? "true" : "false", ret ? "true" : "false", Hex(out, num).c_str(),
        Hex(g_rng.m_state, 32).c_str(), (unsigned long long)g_rng.m_counter);
}

} // namespace

int main()
{
    std::printf("[");
    // Unseeded: MixExtract returns false until a strong mix.
    Mix(false, 0, 0, false);
    Mix(false, 0, 32, false);
    Mix(false, 17, 1, false);
    // SeedEvents with no events: the hash of the empty SHA256, then the re-seeded hasher's.
    Mix(true, 0, 16, false);
    Mix(true, 0, 16, false);
    Mix(false, 64, 32, true);
    // From here on the state reports strongly seeded, whatever `strong` is.
    Mix(false, 5, 31, false);
    // Events: 8 bytes each, so 8 of them fill one SHA256 block and 20 cross two.
    for (uint32_t i = 0; i < 20; ++i) {
        Event(0x01000000u * i + i * 7, (int64_t)(Next() & 0x7fffffffffffffffULL));
    }
    Mix(true, 0, 32, false);
    Mix(true, 3, 8, false);
    Event(0xffffffffu, -1);
    Event(0, (int64_t)0x123456789abcdef0LL);
    Mix(true, 200, 32, false);
    // Inputs across SHA512's 128-byte blocks.
    for (size_t len : {111, 112, 127, 128, 129, 255, 256, 300}) {
        Mix(false, len, len % 33, len % 2 == 0);
    }
    for (uint32_t i = 0; i < 7; ++i) {
        Event(i, (int64_t)(Next() >> 1));
        Mix(i % 3 == 0, i * 9, 32 - i, false);
    }
    std::printf("\n]\n");
}
