// Core's DynamicMemoryUsage for a tip cache holding N fresh outputs, built from the pinned Core
// headers: the real CCoinsMap node (pair<const COutPoint, CCoinsCacheEntry>), PoolAllocator and
// memusage. The hasher is a stand-in with SaltedOutpointHasher's noexcept (no cached hash).
// CH2a a2's witnesses set native Σ beside it (docs/node-program/research/PF1-CH2a-layout-gate.md).
//
// Build against a configured Core tree at the v24 pin (coins.h, memusage.h, pool.h, prevector.h
// are identical at v24.0.0-rc.2-109 and the pin), and run with the output count and script size:
//   S=<core>/src; g++ -std=c++20 -O1 -I$S -I$S/config probe.cpp -o probe
//   ./probe 20000 25    # usage=2263712 (Sol's value)
//   ./probe 50000 25    # usage=5926016: witness (i)
//   ./probe 48701 0     # usage=5926016: witness (ii) after the block, its inputs spent
//   ./probe 48700 9577  # usage=473446016: witness (ii) once its inputs are fetched
#include <coins.h>
#include <memusage.h>
#include <cstdio>
#include <cstdlib>
#include <cstring>
struct H {
    size_t operator()(const COutPoint& o) const noexcept {
        uint64_t x; std::memcpy(&x, o.hash.begin(), 8); return x ^ o.n;
    }
};
using Map = std::unordered_map<COutPoint, CCoinsCacheEntry, H, std::equal_to<COutPoint>,
    PoolAllocator<std::pair<const COutPoint, CCoinsCacheEntry>,
                  sizeof(std::pair<const COutPoint, CCoinsCacheEntry>) + sizeof(void*) * 4>>;
int main(int argc, char** argv) {
    size_t n = std::strtoull(argv[1], nullptr, 10);
    size_t script = std::strtoull(argv[2], nullptr, 10);
    Map::allocator_type::ResourceType res{};
    Map m(0, H{}, std::equal_to<COutPoint>{}, &res);
    size_t cached = 0, peak = 0;
    for (size_t i = 0; i < n; i++) {
        uint256 h; uint64_t v = i * 0x9e3779b97f4a7c15ULL + 1; std::memcpy(h.begin(), &v, 8);
        std::vector<unsigned char> s(script, 0x76); CTxOut out; out.nValue = 1000; out.scriptPubKey = CScript(s.begin(), s.end());
        Coin c(std::move(out), 1, false);
        cached += c.DynamicMemoryUsage();
        m.emplace(std::piecewise_construct, std::forward_as_tuple(h, 0), std::forward_as_tuple(std::move(c)));
        size_t u = memusage::DynamicUsage(m) + cached;
        if (u > peak) peak = u;
    }
    std::printf("n=%zu script=%zu node_pair=%zu buckets=%zu chunks=%zu usage=%zu peak=%zu\n", n, script,
        sizeof(std::pair<const COutPoint, CCoinsCacheEntry>), m.bucket_count(), res.NumAllocatedChunks(),
        memusage::DynamicUsage(m) + cached, peak);
}
