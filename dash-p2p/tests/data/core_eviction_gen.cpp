// Golden eviction decisions from Dash Core v24.0.0-rc.3 `node/eviction.cpp`, compiled with
// GCC 13.3.0 / libstdc++ (Core's guix compiler). Build against the tag's `src/`:
//
//   g++ -std=c++20 -O2 -I<v24.0.0-rc.3>/src core_eviction_gen.cpp -o core_eviction_gen
//   ./core_eviction_gen > core_eviction_trace.txt
//
// Each line is `pattern n seed evicted protected_count protected_hash`: the candidates are
// generated from (pattern, n, seed) by `make` below (mirrored in `tests/eviction.rs`), `evicted`
// is SelectNodeToEvict's id (or `-`), and `protected_*` describe the candidates left, in order,
// after ProtectEvictionCandidatesByRatio on the same input (FNV-1a-64 over the ids as LE u64).
#include <node/eviction.cpp>

#include <climits>
#include <cstdio>

static constexpr int64_t BASE = 1700000000;

struct Rng {
    uint64_t s;
    uint64_t next()
    {
        s += 0x9e3779b97f4a7c15ULL;
        uint64_t z = s;
        z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ULL;
        z = (z ^ (z >> 27)) * 0x94d049bb133111ebULL;
        return z ^ (z >> 31);
    }
    uint64_t below(uint64_t m) { return next() % m; }
};

static const Network NETS[8] = {NET_IPV4, NET_IPV4, NET_IPV4, NET_IPV6, NET_ONION, NET_I2P, NET_CJDNS, NET_UNROUTABLE};

// Patterns: 0 distinct times, equal pings, groups id % 8 (Sol's SB6 r1 case); 1 equal times;
// 2 one group, equal pings; 3 random fields over tiny ranges (heavy ties); 4 random wide fields.
static std::vector<NodeEvictionCandidate> make(int pattern, int n, uint64_t seed)
{
    Rng rng{seed * 1000003 + uint64_t(n) * 7919 + uint64_t(pattern)};
    std::vector<NodeEvictionCandidate> c;
    for (int i = 0; i < n; ++i) {
        NodeEvictionCandidate e{};
        e.id = i;
        e.m_connected = std::chrono::seconds{BASE + i};
        e.m_min_ping_time = std::chrono::microseconds{INT64_MAX};
        e.m_last_block_time = std::chrono::seconds{0};
        e.m_last_tx_time = std::chrono::seconds{0};
        e.fRelevantServices = false;
        e.m_relay_txs = true;
        e.fBloomFilter = false;
        e.nKeyedNetGroup = uint64_t(i % 8);
        e.prefer_evict = false;
        e.m_is_local = false;
        e.m_network = NET_IPV4;
        e.m_noban = false;
        e.m_conn_type = ConnectionType::INBOUND;
        if (pattern == 1) {
            e.m_connected = std::chrono::seconds{BASE};
        } else if (pattern == 2) {
            e.nKeyedNetGroup = 0;
            e.m_min_ping_time = std::chrono::microseconds{1000};
        } else if (pattern >= 3) {
            const bool wide = pattern == 4;
            e.m_connected = std::chrono::seconds{BASE + int64_t(rng.below(wide ? 1 << 20 : 4))};
            const uint64_t ping = rng.below(wide ? 1 << 30 : 4);
            e.m_min_ping_time = std::chrono::microseconds{!wide && ping == 3 ? INT64_MAX : int64_t(ping) * 1000};
            e.m_last_block_time = std::chrono::seconds{int64_t(rng.below(wide ? 1 << 20 : 3))};
            e.m_last_tx_time = std::chrono::seconds{int64_t(rng.below(wide ? 1 << 20 : 3))};
            e.fRelevantServices = rng.below(2);
            e.m_relay_txs = rng.below(2);
            e.fBloomFilter = rng.below(2);
            e.nKeyedNetGroup = wide ? rng.next() : rng.below(4);
            e.prefer_evict = rng.below(8) == 0;
            e.m_is_local = rng.below(10) == 0;
            e.m_network = NETS[rng.below(8)];
            e.m_noban = rng.below(20) == 0;
            e.m_conn_type = rng.below(16) == 0 ? ConnectionType::OUTBOUND_FULL_RELAY : ConnectionType::INBOUND;
        }
        c.push_back(e);
    }
    return c;
}

static void run(int pattern, int n, uint64_t seed)
{
    auto evicted = SelectNodeToEvict(make(pattern, n, seed));
    auto prot = make(pattern, n, seed);
    ProtectEvictionCandidatesByRatio(prot);
    uint64_t h = 0xcbf29ce484222325ULL;
    for (const auto& e : prot) {
        for (int b = 0; b < 8; ++b) {
            h ^= (uint64_t(e.id) >> (8 * b)) & 0xff;
            h *= 0x100000001b3ULL;
        }
    }
    if (evicted) {
        std::printf("%d %d %llu %lld %zu %016llx\n", pattern, n, (unsigned long long)seed, (long long)*evicted, prot.size(), (unsigned long long)h);
    } else {
        std::printf("%d %d %llu - %zu %016llx\n", pattern, n, (unsigned long long)seed, prot.size(), (unsigned long long)h);
    }
}

int main()
{
    for (int pattern : {0, 1, 2}) {
        for (int n : {32, 40, 64, 100}) run(pattern, n, 0);
    }
    for (int pattern : {3, 4}) {
        for (int n = 0; n <= 130; ++n) {
            for (uint64_t seed = 0; seed < 4; ++seed) run(pattern, n, seed);
        }
    }
}
