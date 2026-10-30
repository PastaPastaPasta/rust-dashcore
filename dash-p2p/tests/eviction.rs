//! Eviction decisions against Dash Core's own (`tests/data/core_eviction_trace.txt`, written by
//! `tests/data/core_eviction_gen.cpp` from v24.0.0-rc.3 `node/eviction.cpp` built with GCC
//! 13.3.0). The candidates are rich in ties (equal pings, times and groups), so the chosen peer
//! depends on libstdc++'s `std::sort` order of equal elements (D36).

#![cfg(feature = "inbound")]

use dash_p2p::Network;
use dash_p2p::conn::ConnectionType;
use dash_p2p::inbound::{EvictionCandidate, protect_by_ratio, select_node_to_evict};

const BASE: i64 = 1_700_000_000;

/// splitmix64, as in the generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, m: u64) -> u64 {
        self.next() % m
    }

    fn below_i64(&mut self, m: u64) -> i64 {
        i64::try_from(self.below(m)).unwrap()
    }
}

const NETS: [Network; 8] = [
    Network::Ipv4,
    Network::Ipv4,
    Network::Ipv4,
    Network::Ipv6,
    Network::Onion,
    Network::I2p,
    Network::Cjdns,
    Network::Unroutable,
];

/// The generator's `make`.
fn make(pattern: u64, n: u64, seed: u64) -> Vec<EvictionCandidate> {
    let mut rng = Rng(seed.wrapping_mul(1_000_003).wrapping_add(n * 7919).wrapping_add(pattern));
    (0..n)
        .map(|i| {
            let id = i64::try_from(i).unwrap();
            let mut e = EvictionCandidate {
                id,
                connected: BASE + id,
                min_ping_us: i64::MAX,
                last_block_time: 0,
                last_tx_time: 0,
                relevant_services: false,
                relay_txs: true,
                bloom_filter: false,
                keyed_net_group: i % 8,
                prefer_evict: false,
                is_local: false,
                network: Network::Ipv4,
                noban: false,
                conn_type: ConnectionType::Inbound,
            };
            match pattern {
                1 => e.connected = BASE,
                2 => {
                    e.keyed_net_group = 0;
                    e.min_ping_us = 1000;
                }
                3 | 4 => {
                    let wide = pattern == 4;
                    let pick = |big: u64, small: u64| {
                        if wide {
                            big
                        } else {
                            small
                        }
                    };
                    e.connected = BASE + rng.below_i64(pick(1 << 20, 4));
                    let ping = rng.below_i64(pick(1 << 30, 4));
                    e.min_ping_us = if !wide && ping == 3 {
                        i64::MAX
                    } else {
                        ping * 1000
                    };
                    e.last_block_time = rng.below_i64(pick(1 << 20, 3));
                    e.last_tx_time = rng.below_i64(pick(1 << 20, 3));
                    e.relevant_services = rng.below(2) != 0;
                    e.relay_txs = rng.below(2) != 0;
                    e.bloom_filter = rng.below(2) != 0;
                    e.keyed_net_group = if wide {
                        rng.next()
                    } else {
                        rng.below(4)
                    };
                    e.prefer_evict = rng.below(8) == 0;
                    e.is_local = rng.below(10) == 0;
                    e.network = NETS[usize::try_from(rng.below(8)).unwrap()];
                    e.noban = rng.below(20) == 0;
                    e.conn_type = if rng.below(16) == 0 {
                        ConnectionType::OutboundFullRelay
                    } else {
                        ConnectionType::Inbound
                    };
                }
                _ => {}
            }
            e
        })
        .collect()
}

fn fnv_ids(c: &[EvictionCandidate]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for e in c {
        for b in e.id.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[test]
fn matches_core_eviction_trace() {
    let trace = include_str!("data/core_eviction_trace.txt");
    let mut lines = 0;
    for line in trace.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        let [pattern, n, seed, evicted, kept, hash] = f[..] else {
            panic!("bad line {line:?}");
        };
        let (pattern, n, seed) =
            (pattern.parse().unwrap(), n.parse().unwrap(), seed.parse().unwrap());
        let got = select_node_to_evict(make(pattern, n, seed));
        let want = (evicted != "-").then(|| evicted.parse().unwrap());
        assert_eq!(got, want, "evicted: {line}");
        let mut protected = make(pattern, n, seed);
        protect_by_ratio(&mut protected);
        assert_eq!(protected.len(), kept.parse::<usize>().unwrap(), "protected: {line}");
        assert_eq!(format!("{:016x}", fnv_ids(&protected)), hash, "protected order: {line}");
        lines += 1;
    }
    assert_eq!(lines, 1060);
}
