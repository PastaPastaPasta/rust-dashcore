//! Transcription of Dash Core v24.0.0-rc.3 `src/test/net_peer_eviction_tests.cpp` against
//! `protect_by_ratio` and `select_node_to_evict`.
//!
//! Core draws candidate fields from a seeded `FastRandomContext` and shuffles; its assertions
//! are properties of any draw, so each case here loops over many seeds of a SplitMix64 PRNG.
//! Field mapping: `NodeEvictionCandidate` onto `EvictionCandidate` is one to one (`m_connected`
//! to `connected`, `m_min_ping_time` to `min_ping_us`, ...), with `NET_*` onto `Network`.
//! Core's `ALL_NETWORKS` (`test/util/net.h:123`) is all seven networks.
//! Core's candidate has no Dash-only fields to neutralise: masternode protection lives in
//! `NetCore::eviction_candidates`, outside the functions under test.

use std::collections::HashSet;

use super::*;
use crate::conn::ConnectionType;
use crate::netaddr::Network;

/// SplitMix64: a small deterministic stand-in for Core's `FastRandomContext`.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Core's `randrange(n)`.
    fn randrange(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// Core's `randbool()`.
    fn randbool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }

    /// Core's `Shuffle` (Fisher-Yates).
    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = self.randrange(i as u64 + 1) as usize;
            v.swap(i, j);
        }
    }
}

/// Core's `ALL_NETWORKS` (`test/util/net.h:123`).
const ALL_NETWORKS: [Network; 7] = [
    Network::Unroutable,
    Network::Ipv4,
    Network::Ipv6,
    Network::Onion,
    Network::I2p,
    Network::Cjdns,
    Network::Internal,
];

/// Seeds per case; `peer_eviction_test` does 200 sizes (about 1800 evictions) a seed, so it gets fewer.
const PROTECTION_SEEDS: u64 = 300;
const EVICTION_SEEDS: u64 = 10;

// Core: src/test/util/net.cpp:123-146 GetRandomNodeEvictionCandidates
fn get_random_node_eviction_candidates(n: i64, rng: &mut SplitMix64) -> Vec<EvictionCandidate> {
    (0..n)
        .map(|id| EvictionCandidate {
            id,
            connected: rng.randrange(100) as i64,
            min_ping_us: rng.randrange(100) as i64,
            last_block_time: rng.randrange(100) as i64,
            last_tx_time: rng.randrange(100) as i64,
            relevant_services: rng.randbool(),
            relay_txs: rng.randbool(),
            bloom_filter: rng.randbool(),
            keyed_net_group: rng.randrange(100),
            prefer_evict: rng.randbool(),
            is_local: rng.randbool(),
            network: ALL_NETWORKS[rng.randrange(ALL_NETWORKS.len() as u64) as usize],
            noban: false,
            conn_type: ConnectionType::Inbound,
        })
        .collect()
}

fn set(ids: &[i64]) -> HashSet<i64> {
    ids.iter().copied().collect()
}

// Core: src/test/net_peer_eviction_tests.cpp:20-60 IsProtected
fn is_protected(
    num_peers: i64,
    candidate_setup_fn: impl Fn(&mut EvictionCandidate),
    protected_peer_ids: &[i64],
    unprotected_peer_ids: &[i64],
    rng: &mut SplitMix64,
) -> bool {
    let protected_peer_ids = set(protected_peer_ids);
    let unprotected_peer_ids = set(unprotected_peer_ids);
    let mut candidates = get_random_node_eviction_candidates(num_peers, rng);
    for candidate in &mut candidates {
        candidate_setup_fn(candidate);
    }
    rng.shuffle(&mut candidates);

    let size = candidates.len();
    let expected = size - size / 2; // Expect half the candidates will be protected.
    protect_by_ratio(&mut candidates);
    assert_eq!(candidates.len(), expected);

    let mut unprotected_count = 0;
    for candidate in &candidates {
        if protected_peer_ids.contains(&candidate.id) {
            // This peer should have been removed from the eviction candidates.
            eprintln!("expected candidate to be protected: {}", candidate.id);
            return false;
        }
        if unprotected_peer_ids.contains(&candidate.id) {
            // This peer remains in the eviction candidates, as expected.
            unprotected_count += 1;
        }
    }

    let is_protected = unprotected_count == unprotected_peer_ids.len();
    if !is_protected {
        eprintln!(
            "unprotected: expected {}, actual {}",
            unprotected_peer_ids.len(),
            unprotected_count
        );
    }
    is_protected
}

/// `BOOST_CHECK(IsProtected(...))`, naming the Core source line on failure.
macro_rules! check_protected {
    ($seed:expr, $line:expr, $rng:expr, $n:expr, $setup:expr, $prot:expr, $unprot:expr) => {
        assert!(
            is_protected($n, $setup, $prot, $unprot, $rng),
            "Core net_peer_eviction_tests.cpp:{} failed (seed {})",
            $line,
            $seed
        );
    };
}

// Core: src/test/net_peer_eviction_tests.cpp:62-570 peer_protection_test
#[test]
fn peer_protection_test() {
    for seed in 0..PROTECTION_SEEDS {
        let rng = &mut SplitMix64::new(seed);
        let num_peers: i64 = 12;

        // Expect half of the peers with greatest uptime (the lowest m_connected)
        // to be protected from eviction.
        check_protected!(
            seed,
            69,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = false;
                c.network = Network::Ipv4;
            },
            &[0, 1, 2, 3, 4, 5],
            &[6, 7, 8, 9, 10, 11]
        );

        // Verify in the opposite direction.
        check_protected!(
            seed,
            80,
            rng,
            num_peers,
            |c| {
                c.connected = num_peers - c.id;
                c.is_local = false;
                c.network = Network::Ipv6;
            },
            &[6, 7, 8, 9, 10, 11],
            &[0, 1, 2, 3, 4, 5]
        );

        // Test protection of onion, localhost, and I2P peers...

        // Expect 1/4 onion peers to be protected from eviction,
        // if no localhost, I2P, or CJDNS peers.
        check_protected!(
            seed,
            94,
            rng,
            num_peers,
            |c| {
                c.is_local = false;
                c.network = if c.id == 3 || c.id == 8 || c.id == 9 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[3, 8, 9],
            &[]
        );

        // Expect 1/4 onion peers and 1/4 of the other peers to be protected,
        // sorted by longest uptime (lowest m_connected), if no localhost, I2P or CJDNS peers.
        check_protected!(
            seed,
            105,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = false;
                c.network = if c.id == 3 || c.id > 7 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 3, 8, 9],
            &[4, 5, 6, 7, 10, 11]
        );

        // Expect 1/4 localhost peers to be protected from eviction,
        // if no onion, I2P, or CJDNS peers.
        check_protected!(
            seed,
            117,
            rng,
            num_peers,
            |c| {
                c.is_local = c.id == 1 || c.id == 9 || c.id == 11;
                c.network = Network::Ipv4;
            },
            &[1, 9, 11],
            &[]
        );

        // Expect 1/4 localhost peers and 1/4 of the other peers to be protected,
        // sorted by longest uptime (lowest m_connected), if no onion, I2P, or CJDNS peers.
        check_protected!(
            seed,
            128,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = c.id > 6;
                c.network = Network::Ipv6;
            },
            &[0, 1, 2, 7, 8, 9],
            &[3, 4, 5, 6, 10, 11]
        );

        // Expect 1/4 I2P peers to be protected from eviction,
        // if no onion, localhost, or CJDNS peers.
        check_protected!(
            seed,
            140,
            rng,
            num_peers,
            |c| {
                c.is_local = false;
                c.network = if c.id == 2 || c.id == 7 || c.id == 10 {
                    Network::I2p
                } else {
                    Network::Ipv4
                };
            },
            &[2, 7, 10],
            &[]
        );

        // Expect 1/4 I2P peers and 1/4 of the other peers to be protected, sorted
        // by longest uptime (lowest m_connected), if no onion, localhost, or CJDNS peers.
        check_protected!(
            seed,
            151,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = false;
                c.network = if c.id == 4 || c.id > 8 {
                    Network::I2p
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 4, 9, 10],
            &[3, 5, 6, 7, 8, 11]
        );

        // Expect 1/4 CJDNS peers to be protected from eviction,
        // if no onion, localhost, or I2P peers.
        check_protected!(
            seed,
            163,
            rng,
            num_peers,
            |c| {
                c.is_local = false;
                c.network = if c.id == 2 || c.id == 7 || c.id == 10 {
                    Network::Cjdns
                } else {
                    Network::Ipv4
                };
            },
            &[2, 7, 10],
            &[]
        );

        // Expect 1/4 CJDNS peers and 1/4 of the other peers to be protected, sorted
        // by longest uptime (lowest m_connected), if no onion, localhost, or I2P peers.
        check_protected!(
            seed,
            174,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = false;
                c.network = if c.id == 4 || c.id > 8 {
                    Network::Cjdns
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 4, 9, 10],
            &[3, 5, 6, 7, 8, 11]
        );

        // Tests with 2 networks...

        // Combined test: expect having 1 localhost and 1 onion peer out of 4 to
        // protect 1 localhost, 0 onion and 1 other peer, sorted by longest uptime;
        // stable sort breaks tie with array order of localhost first.
        // (As in Core, the localhost id is 4, which does not exist among 4 peers.)
        check_protected!(
            seed,
            189,
            rng,
            4,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 4;
                c.network = if c.id == 3 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 4],
            &[1, 2]
        );

        // Combined test: expect having 1 localhost and 1 onion peer out of 7 to
        // protect 1 localhost, 0 onion, and 2 other peers (3 total), sorted by
        // uptime; stable sort breaks tie with array order of localhost first.
        check_protected!(
            seed,
            202,
            rng,
            7,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 6;
                c.network = if c.id == 5 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 6],
            &[2, 3, 4, 5]
        );

        // Combined test: expect having 1 localhost and 1 onion peer out of 8 to
        // protect protect 1 localhost, 1 onion and 2 other peers (4 total), sorted
        // by uptime; stable sort breaks tie with array order of localhost first.
        check_protected!(
            seed,
            215,
            rng,
            8,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 6;
                c.network = if c.id == 5 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 5, 6],
            &[2, 3, 4, 7]
        );

        // Combined test: expect having 3 localhost and 3 onion peers out of 12 to
        // protect 2 localhost and 1 onion, plus 3 other peers, sorted by longest
        // uptime; stable sort breaks ties with the array order of localhost first.
        check_protected!(
            seed,
            228,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 6 || c.id == 9 || c.id == 11;
                c.network = if c.id == 7 || c.id == 8 || c.id == 10 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 6, 7, 9],
            &[3, 4, 5, 8, 10, 11]
        );

        // Combined test: expect having 4 localhost and 1 onion peer out of 12 to
        // protect 2 localhost and 1 onion, plus 3 other peers, sorted by longest uptime.
        check_protected!(
            seed,
            240,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = c.id > 4 && c.id < 9;
                c.network = if c.id == 10 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 2, 5, 6, 10],
            &[3, 4, 7, 8, 9, 11]
        );

        // Combined test: expect having 4 localhost and 2 onion peers out of 16 to
        // protect 2 localhost and 2 onions, plus 4 other peers, sorted by longest uptime.
        check_protected!(
            seed,
            252,
            rng,
            16,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 6 || c.id == 9 || c.id == 11 || c.id == 12;
                c.network = if c.id == 8 || c.id == 10 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 3, 6, 8, 9, 10],
            &[4, 5, 7, 11, 12, 13, 14, 15]
        );

        // Combined test: expect having 5 localhost and 1 onion peer out of 16 to
        // protect 3 localhost (recovering the unused onion slot), 1 onion, and 4
        // others, sorted by longest uptime.
        check_protected!(
            seed,
            265,
            rng,
            16,
            |c| {
                c.connected = c.id;
                c.is_local = c.id > 10;
                c.network = if c.id == 10 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 2, 3, 10, 11, 12, 13],
            &[4, 5, 6, 7, 8, 9, 14, 15]
        );

        // Combined test: expect having 1 localhost and 4 onion peers out of 16 to
        // protect 1 localhost and 3 onions (recovering the unused localhost slot),
        // plus 4 others, sorted by longest uptime.
        check_protected!(
            seed,
            278,
            rng,
            16,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 15;
                c.network = if c.id > 6 && c.id < 11 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 3, 7, 8, 9, 15],
            &[5, 6, 10, 11, 12, 13, 14]
        );

        // Combined test: expect having 2 onion and 4 I2P out of 12 peers to protect
        // 2 onion (prioritized for having fewer candidates) and 1 I2P, plus 3
        // others, sorted by longest uptime.
        check_protected!(
            seed,
            291,
            rng,
            num_peers,
            |c| {
                c.connected = c.id;
                c.is_local = false;
                c.network = if c.id == 8 || c.id == 10 {
                    Network::Onion
                } else if c.id == 6 || c.id == 9 || c.id == 11 || c.id == 12 {
                    Network::I2p
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 2, 6, 8, 10],
            &[3, 4, 5, 7, 9, 11]
        );

        // Tests with 3 networks...

        // Combined test: expect having 1 localhost, 1 I2P and 1 onion peer out of 4
        // to protect 1 I2P, 0 localhost, 0 onion and 1 other peer (2 total), sorted
        // by longest uptime; stable sort breaks tie with array order of I2P first.
        check_protected!(
            seed,
            312,
            rng,
            4,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 2;
                c.network = if c.id == 3 {
                    Network::I2p
                } else if c.id == 1 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 3],
            &[1, 2]
        );

        // Combined test: expect having 1 localhost, 1 I2P and 1 onion peer out of 7
        // to protect 1 I2P, 0 localhost, 0 onion and 2 other peers (3 total) sorted
        // by longest uptime; stable sort breaks tie with array order of I2P first.
        check_protected!(
            seed,
            331,
            rng,
            7,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 4;
                c.network = if c.id == 6 {
                    Network::I2p
                } else if c.id == 5 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 6],
            &[2, 3, 4, 5]
        );

        // Combined test: expect having 1 localhost, 1 I2P and 1 onion peer out of 8
        // to protect 1 I2P, 1 localhost, 0 onion and 2 other peers (4 total) sorted
        // by uptime; stable sort breaks tie with array order of I2P then localhost.
        check_protected!(
            seed,
            350,
            rng,
            8,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 6;
                c.network = if c.id == 5 {
                    Network::I2p
                } else if c.id == 4 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 5, 6],
            &[2, 3, 4, 7]
        );

        // Combined test: expect having 4 localhost, 2 I2P, and 2 onion peers out of
        // 16 to protect 1 localhost, 2 I2P, and 1 onion (4/16 total), plus 4 others
        // for 8 total, sorted by longest uptime.
        check_protected!(
            seed,
            369,
            rng,
            16,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 6 || c.id > 11;
                c.network = if c.id == 7 || c.id == 11 {
                    Network::I2p
                } else if c.id == 9 || c.id == 10 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 2, 3, 6, 7, 9, 11],
            &[4, 5, 8, 10, 12, 13, 14, 15]
        );

        // Combined test: expect having 1 localhost, 8 I2P and 1 onion peer out of
        // 24 to protect 1, 4, and 1 (6 total), plus 6 others for 12/24 total,
        // sorted by longest uptime.
        check_protected!(
            seed,
            388,
            rng,
            24,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 12;
                c.network = if c.id > 14 && c.id < 23 {
                    // 4 protected instead of usual 2
                    Network::I2p
                } else if c.id == 23 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 3, 4, 5, 12, 15, 16, 17, 18, 23],
            &[6, 7, 8, 9, 10, 11, 13, 14, 19, 20, 21, 22]
        );

        // Combined test: expect having 1 localhost, 3 I2P and 6 onion peers out of
        // 24 to protect 1, 3, and 2 (6 total, I2P has fewer candidates and so gets the
        // unused localhost slot), plus 6 others for 12/24 total, sorted by longest uptime.
        check_protected!(
            seed,
            407,
            rng,
            24,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 15;
                c.network = if c.id == 12 || c.id == 14 || c.id == 17 {
                    Network::I2p
                } else if c.id > 17 {
                    // 4 protected instead of usual 2
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 2, 3, 4, 5, 12, 14, 15, 17, 18, 19],
            &[6, 7, 8, 9, 10, 11, 13, 16, 20, 21, 22, 23]
        );

        // Combined test: expect having 1 localhost, 7 I2P and 4 onion peers out of
        // 24 to protect 1 localhost, 2 I2P, and 3 onions (6 total), plus 6 others
        // for 12/24 total, sorted by longest uptime.
        check_protected!(
            seed,
            426,
            rng,
            24,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 13;
                c.network = if c.id > 16 {
                    Network::I2p
                } else if c.id == 12 || c.id == 14 || c.id == 15 || c.id == 16 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 3, 4, 5, 12, 13, 14, 15, 17, 18],
            &[6, 7, 8, 9, 10, 11, 16, 19, 20, 21, 22, 23]
        );

        // Combined test: expect having 8 localhost, 4 CJDNS, and 3 onion peers out
        // of 24 to protect 2 of each (6 total), plus 6 others for 12/24 total,
        // sorted by longest uptime.
        check_protected!(
            seed,
            445,
            rng,
            24,
            |c| {
                c.connected = c.id;
                c.is_local = c.id > 15;
                c.network = if c.id > 10 && c.id < 15 {
                    Network::Cjdns
                } else if c.id > 6 && c.id < 10 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 2, 3, 4, 5, 7, 8, 11, 12, 16, 17],
            &[6, 9, 10, 13, 14, 15, 18, 19, 20, 21, 22, 23]
        );

        // Tests with 4 networks...

        // Combined test: expect having 1 CJDNS, 1 I2P, 1 localhost and 1 onion peer
        // out of 5 to protect 1 CJDNS, 0 I2P, 0 localhost, 0 onion and 1 other peer
        // (2 total), sorted by longest uptime; stable sort breaks tie with array
        // order of CJDNS first.
        check_protected!(
            seed,
            467,
            rng,
            5,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 3;
                c.network = if c.id == 4 {
                    Network::Cjdns
                } else if c.id == 1 {
                    Network::I2p
                } else if c.id == 2 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 4],
            &[1, 2, 3]
        );

        // Combined test: expect having 1 CJDNS, 1 I2P, 1 localhost and 1 onion peer
        // out of 7 to protect 1 CJDNS, 0, I2P, 0 localhost, 0 onion and 2 other
        // peers (3 total) sorted by longest uptime; stable sort breaks tie with
        // array order of CJDNS first.
        check_protected!(
            seed,
            489,
            rng,
            7,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 4;
                c.network = if c.id == 6 {
                    Network::Cjdns
                } else if c.id == 5 {
                    Network::I2p
                } else if c.id == 3 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 6],
            &[2, 3, 4, 5]
        );

        // Combined test: expect having 1 CJDNS, 1 I2P, 1 localhost and 1 onion peer
        // out of 8 to protect 1 CJDNS, 1 I2P, 0 localhost, 0 onion and 2 other
        // peers (4 total) sorted by longest uptime; stable sort breaks tie with
        // array order of CJDNS first.
        check_protected!(
            seed,
            511,
            rng,
            8,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 3;
                c.network = if c.id == 5 {
                    Network::Cjdns
                } else if c.id == 6 {
                    Network::I2p
                } else if c.id == 3 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 5, 6],
            &[2, 3, 4, 7]
        );

        // Combined test: expect having 2 CJDNS, 2 I2P, 4 localhost, and 2 onion
        // peers out of 16 to protect 1 CJDNS, 1 I2P, 1 localhost, 1 onion (4/16
        // total), plus 4 others for 8 total, sorted by longest uptime.
        check_protected!(
            seed,
            532,
            rng,
            16,
            |c| {
                c.connected = c.id;
                c.is_local = c.id > 5;
                c.network = if c.id == 11 || c.id == 15 {
                    Network::Cjdns
                } else if c.id == 10 || c.id == 14 {
                    Network::I2p
                } else if c.id == 8 || c.id == 9 {
                    Network::Onion
                } else {
                    Network::Ipv4
                };
            },
            &[0, 1, 2, 3, 6, 8, 10, 11],
            &[4, 5, 7, 9, 12, 13, 14, 15]
        );

        // Combined test: expect having 6 CJDNS, 1 I2P, 1 localhost, and 4 onion
        // peers out of 24 to protect 2 CJDNS, 1 I2P, 1 localhost, and 2 onions (6
        // total), plus 6 others for 12/24 total, sorted by longest uptime.
        check_protected!(
            seed,
            553,
            rng,
            24,
            |c| {
                c.connected = c.id;
                c.is_local = c.id == 13;
                c.network = if c.id > 17 {
                    Network::Cjdns
                } else if c.id == 17 {
                    Network::I2p
                } else if c.id == 12 || c.id == 14 || c.id == 15 || c.id == 16 {
                    Network::Onion
                } else {
                    Network::Ipv6
                };
            },
            &[0, 1, 2, 3, 4, 5, 12, 13, 14, 17, 18, 19],
            &[6, 7, 8, 9, 10, 11, 15, 16, 20, 21, 22, 23]
        );
    }
}

// Core: src/test/net_peer_eviction_tests.cpp:572-581 IsEvicted (candidates overload)
// Returns true if any of the node ids in node_ids are selected for eviction.
fn is_evicted_candidates(
    mut candidates: Vec<EvictionCandidate>,
    node_ids: &[i64],
    rng: &mut SplitMix64,
) -> bool {
    rng.shuffle(&mut candidates);
    match select_node_to_evict(candidates) {
        None => false,
        Some(id) => node_ids.contains(&id),
    }
}

// Core: src/test/net_peer_eviction_tests.cpp:583-593 IsEvicted (setup overload)
// Create number_of_nodes random nodes, apply setup function candidate_setup_fn,
// apply eviction logic and then return true if any of the node ids in node_ids
// are selected for eviction.
fn is_evicted(
    number_of_nodes: i64,
    candidate_setup_fn: impl Fn(&mut EvictionCandidate),
    node_ids: &[i64],
    rng: &mut SplitMix64,
) -> bool {
    let mut candidates = get_random_node_eviction_candidates(number_of_nodes, rng);
    for candidate in &mut candidates {
        candidate_setup_fn(candidate);
    }
    is_evicted_candidates(candidates, node_ids, rng)
}

// Core: src/test/net_peer_eviction_tests.cpp:595-683 peer_eviction_test
#[test]
fn peer_eviction_test() {
    for seed in 0..EVICTION_SEEDS {
        let rng = &mut SplitMix64::new(seed);
        for number_of_nodes in 0..200i64 {
            let n = number_of_nodes;
            let msg = |line: u32| format!("Core line {line}, seed {seed}, nodes {n}");

            // Four nodes with the highest keyed netgroup values should be
            // protected from eviction.
            assert!(
                !is_evicted(n, |c| c.keyed_net_group = (n - c.id) as u64, &[0, 1, 2, 3], rng),
                "{}",
                msg(602)
            );

            // Eight nodes with the lowest minimum ping time should be protected
            // from eviction.
            assert!(
                !is_evicted(n, |c| c.min_ping_us = c.id, &[0, 1, 2, 3, 4, 5, 6, 7], rng),
                "{}",
                msg(610)
            );

            // Four nodes that most recently sent us novel transactions accepted
            // into our mempool should be protected from eviction.
            assert!(
                !is_evicted(n, |c| c.last_tx_time = n - c.id, &[0, 1, 2, 3], rng),
                "{}",
                msg(618)
            );

            // Up to eight non-tx-relay peers that most recently sent us novel
            // blocks should be protected from eviction.
            assert!(
                !is_evicted(
                    n,
                    |c| {
                        c.last_block_time = n - c.id;
                        if c.id <= 7 {
                            c.relay_txs = false;
                            c.relevant_services = true;
                        }
                    },
                    &[0, 1, 2, 3, 4, 5, 6, 7],
                    rng
                ),
                "{}",
                msg(626)
            );

            // Four peers that most recently sent us novel blocks should be
            // protected from eviction.
            assert!(
                !is_evicted(n, |c| c.last_block_time = n - c.id, &[0, 1, 2, 3], rng),
                "{}",
                msg(638)
            );

            // Combination of the previous two tests.
            assert!(
                !is_evicted(
                    n,
                    |c| {
                        c.last_block_time = n - c.id;
                        if c.id <= 7 {
                            c.relay_txs = false;
                            c.relevant_services = true;
                        }
                    },
                    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
                    rng
                ),
                "{}",
                msg(645)
            );

            // Combination of all tests above.
            assert!(
                !is_evicted(
                    n,
                    |c| {
                        c.keyed_net_group = (n - c.id) as u64; // 4 protected
                        c.min_ping_us = c.id; // 8 protected
                        c.last_tx_time = n - c.id; // 4 protected
                        c.last_block_time = n - c.id; // 4 protected
                    },
                    &(0..20).collect::<Vec<_>>(),
                    rng
                ),
                "{}",
                msg(656)
            );

            // An eviction is expected given >= 29 random eviction candidates. The eviction logic
            // protects at most four peers by net group, eight by lowest ping time, four by last
            // time of novel tx, up to eight non-tx-relay peers by last novel block time, and four
            // more peers by last novel block time.
            if n >= 29 {
                assert!(
                    select_node_to_evict(get_random_node_eviction_candidates(n, rng)).is_some(),
                    "{}",
                    msg(668)
                );
            }

            // No eviction is expected given <= 20 random eviction candidates. The eviction logic
            // protects at least four peers by net group, eight by lowest ping time, four by last
            // time of novel tx and four peers by last novel block time.
            if n <= 20 {
                assert!(
                    select_node_to_evict(get_random_node_eviction_candidates(n, rng)).is_none(),
                    "{}",
                    msg(675)
                );
            }

            // Cases left to test (as in Core):
            // * "If any remaining peers are preferred for eviction consider only them. [...]"
            // * "Identify the network group with the most connections and youngest member. [...]"
        }
    }
}
