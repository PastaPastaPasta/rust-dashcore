# Test data

- `asmap.raw`: Dash Core `src/test/data/asmap.raw` at tag `v24.0.0-rc.3`
  (commit `0364ed626ec2eaa518bf1ae4a0566ce638c7fe18`), byte for byte
  (`git show v24.0.0-rc.3:src/test/data/asmap.raw`; sha256
  `55dcec00c72b8a33a271dad20271e14cfbe5526aa1dc12e2559f30c376d85a35`). It is the mock
  mapping `addrman_tests.cpp` uses (250.0.0.0/8 → AS1000, 101.1–8.0.0/16 → AS1–8); the
  AddrMan unit tests in `src/addrman/tests.rs` read it. MIT, from Dash Core.
- `core_addrman_gen.cpp`, `core_peers.dat`, `core_addrman_trace.txt`: golden AddrMan output from
  Dash Core itself. `core_addrman_gen.cpp` is linked against a v24.0.0-rc.3 build tree (whose
  `addrman.cpp`/`addrman_impl.h`/`addrdb.cpp` are identical to the tag) and uses glibc and
  libstdc++. In deterministic mode with check ratio 100 and `NOW = 1700000000`, it runs 260
  Add/Good/Attempt ops, `ResolveCollisions`, `Select` and `GetAddr` and records each result in
  the trace. It then writes `core_peers.dat` (mainnet magic) and records the SHA256d of the
  file Core writes after three loads: a plain reload, a reload with `asmap.raw` (re-bucketing),
  and a load of a copy with five entries patched to `0.0.0.0` (pruning). sha256:
  `core_peers.dat` `84a8515a095dcadf82b1d3aaabb9eb70c5510964dbebdbe527ab08339153e16d`,
  `core_addrman_trace.txt` `f1ba21575a2acddecf822b99a5e5b167f16abda444954a1662874f26afb1a9ac`.
  The `core_golden_trace` test in `src/addrman/tests.rs` replays them.
- `core_eviction_gen.cpp`, `core_eviction_trace.txt`: golden eviction decisions from Dash Core
  v24.0.0-rc.3 `node/eviction.cpp`, compiled into the generator (`#include <node/eviction.cpp>`
  with `-I` on the tag's `src/`) by GCC 13.3.0 (`docker gcc:13.3.0`, Core's guix compiler;
  the local GCC 15.2 writes the same file). 1060 candidate sets, rich in equal pings, times and
  groups, each with `SelectNodeToEvict`'s choice and the order `ProtectEvictionCandidatesByRatio`
  leaves. The `tests/eviction.rs` test rebuilds the candidates with the same generator. sha256:
  `core_eviction_trace.txt` `daccfa63cb3d9d9aaf27bd549cb93019c9b60e42caee855b354c3015829a24bd`.
- `libstdcxx_sort_ref.cpp`, `libstdcxx_sort_trace.txt`: libstdc++'s `std::sort` (GCC 13.3.0,
  `docker gcc:13.3.0`, static) on the 1749 cases `tests/stdsort.rs` builds: random keys over
  1 to 2⁶⁴ values, sorted, reversed and duplicated runs, McIlroy's antiqsort (heap fallback)
  and two-key comparators. Each line holds the comparator-call count, a hash of the call
  sequence and a hash of the final order. Rerun with `DASH_P2P_SORT_REF` to regenerate. sha256:
  `libstdcxx_sort_trace.txt` `1977fb40c80a75cb15b1b4bec4f87052895d8d9f8e424fd2fb916b1aca2c948b`.
- `core_banlist.json`, `core_banlist_empty.json`: `regtest/banlist.json` as Dash Core
  v24.0.0-rc.3's release `dashd` (`DASHVERSION=24.0.0-rc.3 contrib/setup-dashd.py`) writes it.
  The empty file is what a fresh `-regtest -listen=0` datadir holds after start and `stop`
  (`LoadBanlist` recreates it). The other is from a fresh datadir after `setmocktime
  1700000000`, then `setban 1.2.3.4 add 4102444800 true`, `setban 2a01:4f8::1 add 3600`,
  `setban 10.0.0.0/8 add 1800000000 true`, `setban 2001:db8:1::/48 add 4102444802 true`,
  `setban 192.168.0.0/255.255.0.0 add 1700086400 true`, `setban 9.8.7.6/32 add 1750000000
  true`, `setban 2a02:1234::/ffff:ffff:: add 86400` and `stop`. `tests/banlist_json.rs` makes
  the same bans and compares bytes. sha256: `core_banlist.json` `272b913670a36da4fcc724f45202982093d121517c0c2cf6cbd4c7da35124627`,
  `core_banlist_empty.json` `b6df734c272c270f7480b6e463da1777fe4308e5c5ec2895bf9f5c5cd7e56aa1`.
