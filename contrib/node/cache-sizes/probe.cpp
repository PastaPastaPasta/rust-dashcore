// Core's CalculateCacheSizes (node/caches.cpp), linked from a Core build tree at the v24 pin, for
// the cache_sizes vectors of dashd-chainstate (CH2 §2.5, §7.1 a3). gen.py builds and runs it.
//
// Each stdin line is one configuration: Core options (`-dbcache=300`, `-txindex=0`, ...) set with
// ArgsManager::ForceSetArg, and `n_indexes=N` (the enabled block filter types). Each output line
// is the eight sizes in bytes: block_tree_db coins_db coins tx_index address_index timestamp_index
// spent_index filter_index.
#include <node/caches.h>
#include <util/system.h>
#include <util/translation.h>

#include <cstdio>
#include <functional>
#include <iostream>
#include <sstream>
#include <string>

const std::function<std::string(const char*)> G_TRANSLATION_FUN = nullptr;

int main() {
    std::string line;
    while (std::getline(std::cin, line)) {
        ArgsManager args;
        size_t n_indexes = 0;
        std::istringstream words(line);
        std::string a;
        while (words >> a) {
            auto eq = a.find('=');
            std::string k = a.substr(0, eq), v = eq == std::string::npos ? "1" : a.substr(eq + 1);
            if (k == "n_indexes") {
                n_indexes = std::stoul(v);
            } else {
                args.ForceSetArg(k, v);
            }
        }
        node::CacheSizes s = node::CalculateCacheSizes(args, n_indexes);
        std::printf("%lld %lld %lld %lld %lld %lld %lld %lld\n", (long long)s.block_tree_db,
                    (long long)s.coins_db, (long long)s.coins, (long long)s.tx_index,
                    (long long)s.address_index, (long long)s.timestamp_index,
                    (long long)s.spent_index, (long long)s.filter_index);
    }
}
