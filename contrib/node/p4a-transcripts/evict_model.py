#!/usr/bin/env python3
"""Checks capture.py's `inbound-evict-groups` design against Core's own eviction code.

Compiles Dash Core's `src/node/eviction.cpp` (v24.0.0-rc.3) into a small harness, replays the
design's connects (each candidate: connect order, ping 0 for absorbed peers and never for the
others, tx relay for the others only, all services relevant, its netgroup), and for every
eviction runs `SelectNodeToEvict` under all 24 orders of the four netgroups' keys. It fails
unless every eviction is the same under all 24 and every set the netgroup protection picks lies
in the absorbed set; it prints the evictions dashd must make. The keys aside, ties still fall to
libstdc++'s unstable `std::sort` over the candidates in connect order (`m_nodes`), so the harness
needs libstdc++ (dashd's release builds use GCC 13.3's, which `dash_p2p::stdsort` ports).

Usage:
  evict_model.py CORE_SRC      (CORE_SRC: Dash Core's `src` directory at v24.0.0-rc.3)
"""

import importlib.util
import pathlib
import subprocess
import sys
import tempfile

HARNESS = r"""
#include <algorithm>
#include <array>
#include <cassert>
#include <chrono>
#include <cstdint>
#include <functional>
#include <iostream>
#include <map>
#include <optional>
#include <vector>
#ifndef __GLIBCXX__
#error "Core's eviction ties fall to libstdc++'s std::sort; build with libstdc++ (g++)"
#endif
enum Network { NET_UNROUTABLE, NET_IPV4, NET_IPV6, NET_ONION, NET_I2P, NET_CJDNS, NET_INTERNAL, NET_MAX };
enum class ConnectionType { INBOUND };
typedef int64_t NodeId;
struct NodeEvictionCandidate {
    NodeId id; std::chrono::seconds m_connected; std::chrono::microseconds m_min_ping_time;
    std::chrono::seconds m_last_block_time; std::chrono::seconds m_last_tx_time; bool fRelevantServices;
    bool m_relay_txs; bool fBloomFilter; uint64_t nKeyedNetGroup; bool prefer_evict; bool m_is_local;
    Network m_network; bool m_noban; ConnectionType m_conn_type;
};
@EVICTION@
// stdin: "n" then n lines "id ping(-1: never) relay group"; prints, per key order, the evicted id
// and the 4 the netgroup protection keeps.
int main() {
    int n;
    while (std::cin >> n) {
        std::vector<NodeEvictionCandidate> base;
        for (int i = 0; i < n; i++) {
            long long id, ping; int relay, group;
            std::cin >> id >> ping >> relay >> group;
            NodeEvictionCandidate c{};
            c.id = id; c.m_connected = std::chrono::seconds(id);
            c.m_min_ping_time = ping < 0 ? std::chrono::microseconds::max() : std::chrono::microseconds(ping);
            c.fRelevantServices = true; c.m_relay_txs = relay; c.nKeyedNetGroup = group;
            c.m_network = NET_IPV4; c.m_conn_type = ConnectionType::INBOUND;
            base.push_back(c);
        }
        std::array<int, 4> order{0, 1, 2, 3};
        do {
            auto v = base;
            for (auto& c : v) c.nKeyedNetGroup = 1000 + order[c.nKeyedNetGroup];
            auto kept = v;
            std::sort(kept.begin(), kept.end(), CompareNetGroupKeyed);
            auto r = SelectNodeToEvict(std::move(v));
            std::cout << (r ? *r : -1);
            for (size_t i = kept.size() - 4; i < kept.size(); i++) std::cout << (i == kept.size() - 4 ? ":" : ",") << kept[i].id;
            std::cout << " ";
        } while (std::next_permutation(order.begin(), order.end()));
        std::cout << std::endl;
    }
}
"""


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    src = pathlib.Path(sys.argv[1]) / "node" / "eviction.cpp"
    eviction = src.read_text().split("#include <vector>", 1)[1]
    spec = importlib.util.spec_from_file_location("p4a_capture", pathlib.Path(__file__).resolve().parent / "capture.py")
    cap = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(cap)
    fill, newcomers = cap.evict_design()
    with tempfile.TemporaryDirectory() as tmp:
        cpp, exe = pathlib.Path(tmp) / "model.cpp", pathlib.Path(tmp) / "model"
        cpp.write_text(HARNESS.replace("@EVICTION@", eviction))
        subprocess.run(["g++", "-O2", "-std=c++20", "-o", str(exe), str(cpp)], check=True)
        peers = {n: (g, absorbed) for n, (g, absorbed) in enumerate(fill + newcomers, 1)}
        table = list(range(1, len(fill) + 1))
        failed = False
        for n in range(len(fill) + 1, len(peers) + 1):
            query = f"{len(table)}\n" + "".join(
                f"{i} {0 if peers[i][1] else -1} {int(not peers[i][1])} {peers[i][0]}\n" for i in table)
            out = subprocess.run([str(exe)], input=query, capture_output=True, text=True, check=True).stdout.split()
            evicted = {int(o.split(":")[0]) for o in out}
            kept = set().union(*(set(map(int, o.split(":")[1].split(","))) for o in out))
            outside = sorted(i for i in kept if not peers[i][1])
            ok = len(evicted) == 1 and not outside and -1 not in evicted
            failed |= not ok
            e = min(evicted)
            print(f"peer {n} connects: dashd evicts {sorted(evicted)} (netgroup {peers[e][0] if e > 0 else '-'})"
                  + ("" if ok else f"  FAIL (netgroup-protected outside the absorbed set: {outside})"))
            if e > 0:
                table.remove(e)
            table.append(n)
        sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
