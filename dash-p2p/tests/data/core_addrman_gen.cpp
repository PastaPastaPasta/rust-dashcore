// Golden AddrMan traces from Dash Core v24.0.0-rc.3 (oracle-shim-v24.0 build tree).
#include <addrman.h>
#include <chainparams.h>
#include <clientversion.h>
#include <hash.h>
#include <netbase.h>
#include <netgroup.h>
#include <streams.h>
#include <util/time.h>
#include <cstdio>
#include <functional>
#include <fstream>
#include <string>
#include <vector>

extern const std::function<std::string(const char*)> G_TRANSLATION_FUN;
const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;
static constexpr int64_t NOW = 1700000000;
static FILE* out;

static CNetAddr IP(const std::string& s) { return *LookupHost(s, false); }
static CService SVC(const std::string& s, uint16_t port) { return *Lookup(s, port, false); }

static void dump(const AddrMan& am, const std::string& path) {
    CAutoFile f(fopen(path.c_str(), "wb"), SER_DISK, CLIENT_VERSION);
    HashedSourceWriter hw{f};
    hw << Params().MessageStart() << am;
    f << hw.GetHash();
}

static void load(AddrMan& am, const std::string& path) {
    CAutoFile f(fopen(path.c_str(), "rb"), SER_DISK, CLIENT_VERSION);
    CHashVerifier<CAutoFile> v(&f);
    unsigned char magic[4];
    v >> magic;
    v >> am;
    uint256 h;
    f >> h;
    if (h != v.GetHash()) throw std::runtime_error("checksum");
}

static void trace(AddrMan& am, const char* tag) {
    for (int i = 0; i < 12; ++i) {
        auto [a, t] = am.Select(i % 3 == 0);
        fprintf(out, "%s select %s %lld\n", tag, a.ToStringAddrPort().c_str(), (long long)TicksSinceEpoch<std::chrono::seconds>(t));
    }
    auto [a4, t4] = am.Select(false, NET_IPV4);
    fprintf(out, "%s select4 %s\n", tag, a4.ToStringAddrPort().c_str());
    for (auto& a : am.GetAddr(40, 23, std::nullopt)) fprintf(out, "%s getaddr %s %u %llu\n", tag, a.ToStringAddrPort().c_str(), (unsigned)TicksSinceEpoch<std::chrono::seconds>(a.nTime), (unsigned long long)a.nServices);
    fprintf(out, "%s size %zu %zu %zu\n", tag, am.Size(), am.Size(std::nullopt, true), am.Size(std::nullopt, false));
}

static std::vector<bool> FromBytes(const std::vector<unsigned char>& b) {
    std::vector<bool> r(b.size() * 8);
    for (size_t i = 0; i < b.size(); ++i) for (int j = 0; j < 8; ++j) r[i * 8 + j] = (b[i] >> j) & 1;
    return r;
}

int main(int argc, char** argv) {
    SelectParams(CBaseChainParams::MAIN);
    SetMockTime(NOW);
    std::string dir = argv[1];
    out = fopen((dir + "/core_trace.txt").c_str(), "w");
    std::ifstream af(argv[2], std::ios::binary);
    std::vector<unsigned char> asmap_raw((std::istreambuf_iterator<char>(af)), {});
    NetGroupManager plain{std::vector<bool>()};
    NetGroupManager asmapped{FromBytes(asmap_raw)};

    AddrMan am(plain, /*deterministic=*/true, /*consistency_check_ratio=*/100);
    for (int i = 0; i < 160; ++i) {
        std::string s = (i % 4 == 0 ? std::string("250.") : std::to_string(1 + i % 200) + ".") + std::to_string(i / 4 % 256) + "." + std::to_string(i % 7) + ".7";
        CAddress addr(SVC(s, 9999 - i % 3), ServiceFlags(i % 2 ? NODE_NETWORK : NODE_NONE), NodeSeconds{std::chrono::seconds{NOW - i}});
        CNetAddr src = IP(std::to_string(2 + i % 13) + "." + std::to_string(i % 5) + ".3.3");
        am.Add({addr}, src, std::chrono::seconds{i % 4 == 1 ? 7200 : 0});
        if (i % 5 == 0) am.Good(addr, NodeSeconds{std::chrono::seconds{NOW}});
        if (i % 7 == 0) am.Attempt(addr, true, NodeSeconds{std::chrono::seconds{NOW - 100}});
    }
    for (int i = 0; i < 100; ++i) {
        std::string s = (i % 4 == 0 ? std::string("250.") : std::to_string(1 + i % 200) + ".") + std::to_string(i / 4 % 256) + "." + std::to_string(i % 7) + ".7";
        CAddress addr(SVC(s, 9999 - i % 3), NODE_NONE, NodeSeconds{std::chrono::seconds{NOW + i}});
        am.Add({addr}, IP(std::to_string(40 + i % 50) + ".9.3.3"), 0s);
    }
    am.ResolveCollisions();
    auto [c, ct] = am.SelectTriedCollision();
    fprintf(out, "ops collision %s\n", c.ToStringAddrPort().c_str());
    trace(am, "ops");
    dump(am, dir + "/core_peers.dat");

    // Reload into a plain addrman (restores bucketing), trace, dump again.
    AddrMan re(plain, true, 100);
    load(re, dir + "/core_peers.dat");
    trace(re, "reload");
    dump(re, dir + "/core_peers_reload.dat");

    // Reload into an asmap addrman: checksum differs, re-bucket by source, prune collisions.
    AddrMan as(asmapped, true, 100);
    load(as, dir + "/core_peers.dat");
    trace(as, "asmap");
    dump(as, dir + "/core_peers_asmap.dat");
    // A pruning load: five new entries patched to 0.0.0.0 are dropped by the prune loop.
    AddrMan pr(plain, true, 100);
    load(pr, dir + "/core_peers_pruned_in.dat");
    trace(pr, "pruned");
    dump(pr, dir + "/core_peers_pruned.dat");
    fclose(out);
}
