// Reference harness for `tests/stdsort.rs`: runs libstdc++ `std::sort` on cases read from
// stdin and reports the comparator calls. Build it with GCC 13.3.0, Dash Core's guix compiler.
//
// Input, one case per line:
//   S n k_0 .. k_{n-1}              elements (key, id = index); comp is key <
//   M n a_0 b_0 .. a_{n-1} b_{n-1}  elements (a, b, id); comp is a <, then b >
// Output, one line per case: `ncmp trace_hash id_0 .. id_{n-1}`, where trace_hash is an
// FNV-1a-64 over the (lhs id, rhs id) of every comparator call (u32 LE), in call order.
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <iostream>
#include <string>
#include <vector>

static uint64_t g_hash;
static uint64_t g_ncmp;

static inline void trace(uint32_t x, uint32_t y) {
    g_ncmp++;
    for (uint32_t w : {x, y}) {
        for (int i = 0; i < 4; i++) {
            g_hash ^= (w >> (8 * i)) & 0xff;
            g_hash *= 0x100000001b3ULL;
        }
    }
}

struct S { int64_t key; uint32_t id; };
struct M { int64_t a; int64_t b; uint32_t id; };

int main() {
    std::ios::sync_with_stdio(false);
    std::string kind;
    std::string out;
    while (std::cin >> kind) {
        size_t n;
        std::cin >> n;
        std::vector<uint32_t> ids;
        g_hash = 0xcbf29ce484222325ULL;
        g_ncmp = 0;
        if (kind == "S") {
            std::vector<S> v(n);
            for (size_t i = 0; i < n; i++) { std::cin >> v[i].key; v[i].id = (uint32_t)i; }
            std::sort(v.begin(), v.end(), [](const S& x, const S& y) {
                trace(x.id, y.id);
                return x.key < y.key;
            });
            for (auto& e : v) ids.push_back(e.id);
        } else if (kind == "M") {
            std::vector<M> v(n);
            for (size_t i = 0; i < n; i++) { std::cin >> v[i].a >> v[i].b; v[i].id = (uint32_t)i; }
            // Shaped like Core's eviction comparators: primary key ascending,
            // tie broken by a second key descending; ties on both remain.
            std::sort(v.begin(), v.end(), [](const M& x, const M& y) {
                trace(x.id, y.id);
                if (x.a != y.a) return x.a < y.a;
                return x.b > y.b;
            });
            for (auto& e : v) ids.push_back(e.id);
        } else {
            std::cerr << "bad kind " << kind << "\n";
            return 2;
        }
        out.clear();
        out += std::to_string(g_ncmp) + " " + std::to_string(g_hash);
        for (uint32_t id : ids) { out += ' '; out += std::to_string(id); }
        out += '\n';
        std::cout << out;
    }
    return 0;
}
