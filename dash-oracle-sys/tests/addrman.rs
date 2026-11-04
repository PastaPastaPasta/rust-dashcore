//! Smoke differential test of the address-manager entry points (ABI 10) for every pin.
//!
//! Expected values are the known answers Dash Core's own unit tests assert at the v24.0
//! pin (`src/test/addrman_tests.cpp`, `src/test/netbase_tests.cpp`, cited by line; see
//! tests/data/PROVENANCE.md) or, for `GetChance`, the documented formula evaluated in
//! Rust (a bit-exactness check of `std::pow` against `f64::powf`). The draw-log tests
//! check the seeded RNG seam: equal seeds and operations give equal draws, in this process
//! and in two fresh ones.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::process::Command;

use dash_oracle_sys::addrman::{NET_IPV4, NET_IPV6};
use dash_oracle_sys::{
    AddrMan, AddrManConfig, AddrManState, AddrOp, Address, Chain, Draw, ErrorKind, LoadOutcome,
    NetAddr, OpResult, Oracle, Service,
};
use dashcore::hashes::{Hash, sha256, sha256d};

fn ip(s: &str) -> NetAddr {
    match s.parse::<Ipv4Addr>() {
        Ok(v4) => NetAddr::ipv4(v4.octets()),
        Err(_) => NetAddr::ipv6(s.parse::<Ipv6Addr>().expect(s).octets()),
    }
}

fn service(s: &str, port: u16) -> Service {
    Service::new(ip(s), port)
}

/// `CAddress(service, NODE_NONE)` as Core's tests build it (nTime is `TIME_INIT`).
fn address(s: &str, port: u16) -> Address {
    Address {
        time: 100_000_000,
        services: 0,
        service: service(s, port),
    }
}

/// `(HashWriter{} << n).GetHash()` for an int n, the tests' bucket keys.
fn hash_writer_int(n: i32) -> [u8; 32] {
    sha256d::Hash::hash(&n.to_le_bytes()).to_byte_array()
}

fn asmap_raw() -> Vec<u8> {
    let path = format!("{}/tests/data/core-tests/asmap.raw", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).expect(&path)
}

const T: i64 = 1_700_000_000;

fn new_addrman(oracle: &Oracle, config: &AddrManConfig) -> AddrMan {
    oracle.addrman_new(config).expect("addrman_new").expect("asmap accepted")
}

fn run(am: &mut AddrMan, op: AddrOp, mocktime: i64) -> OpResult {
    am.op(&op, mocktime).unwrap_or_else(|e| panic!("{op:?}: {e}")).result
}

fn selected(result: OpResult) -> Service {
    match result {
        OpResult::Selected {
            addr,
            ..
        } => addr.service,
        other => panic!("not a selection: {other:?}"),
    }
}

#[test]
fn buckets_match_core_known_answers() {
    let asmap = asmap_raw();
    for oracle in Oracle::all() {
        let (key1, key2) = (hash_writer_int(1), hash_writer_int(2));
        let b = |addr: &Service, src: &NetAddr, key: &[u8; 32], asmap: &[u8]| {
            oracle.addrman_bucket(key, addr, src, asmap).unwrap().expect("asmap accepted")
        };
        // caddrinfo_get_tried_bucket_legacy (addrman_tests.cpp:452-463)
        let (a1, a2, s1) =
            (service("250.1.1.1", 8333), service("250.1.1.1", 9999), ip("250.1.1.1"));
        assert_eq!(b(&a1, &s1, &key1, &[]).tried_bucket, 40, "{}", oracle.pin());
        assert_ne!(b(&a1, &s1, &key1, &[]).tried_bucket, b(&a1, &s1, &key2, &[]).tried_bucket);
        assert_ne!(b(&a1, &s1, &key1, &[]).tried_bucket, b(&a2, &s1, &key1, &[]).tried_bucket);
        // caddrinfo_get_tried_bucket (addrman_tests.cpp:577-591), with asmap.raw
        assert_eq!(b(&a1, &s1, &key1, &asmap).tried_bucket, 236);
        // caddrinfo_get_new_bucket_legacy (addrman_tests.cpp:503-515) and
        // caddrinfo_get_new_bucket (addrman_tests.cpp:631-646)
        let (a1, s1) = (service("250.1.2.1", 8333), ip("250.1.2.1"));
        assert_eq!(b(&a1, &s1, &key1, &[]).new_bucket, 786);
        assert_eq!(b(&a1, &s1, &key1, &asmap).new_bucket, 795);
        let buckets = b(&a1, &s1, &key1, &[]);
        assert!(buckets.pos_new < 64 && buckets.pos_tried < 64);
    }
}

#[test]
fn netgroups_match_core_known_answers() {
    // netbase_getgroup (netbase_tests.cpp:329-339): Core's Network values, then prefix bytes.
    let cases: [(&str, &[u8]); 9] = [
        ("127.0.0.1", &[0]),
        ("10.0.0.1", &[0]),
        ("169.254.1.1", &[0]),
        ("1.2.3.4", &[NET_IPV4, 1, 2]),
        ("::ffff:0:102:304", &[NET_IPV4, 1, 2]),
        ("64:ff9b::102:304", &[NET_IPV4, 1, 2]),
        ("2002:102:304:9999:9999:9999:9999:9999", &[NET_IPV4, 1, 2]),
        ("2001:470:abcd:9999:9999:9999:9999:9999", &[NET_IPV6, 32, 1, 4, 112, 175]),
        ("2001:2001:9999:9999:9999:9999:9999:9999", &[NET_IPV6, 32, 1, 32, 1]),
    ];
    for oracle in Oracle::all() {
        for (addr, group) in cases {
            let (got, asn) = oracle.netgroup(&ip(addr), &[]).unwrap().expect("no asmap");
            assert_eq!(got, group, "{} {addr}", oracle.pin());
            assert_eq!(asn, 0, "no asmap maps no AS");
        }
        // With asmap.raw an address it maps is grouped by its AS (addrman_serialization's
        // 250.1.1.1 changes bucket), and a refused asmap is a verdict.
        let (group, asn) = oracle.netgroup(&ip("250.1.1.1"), &asmap_raw()).unwrap().unwrap();
        assert_ne!(asn, 0);
        assert_eq!(
            group,
            [&[NET_IPV6][..], &asn.to_le_bytes()].concat(),
            "IPv4 and IPv6 of one AS share a group"
        );
        assert_eq!(oracle.netgroup(&ip("1.2.3.4"), &[0xff]).unwrap(), None);
        assert_eq!(
            oracle
                .addrman_bucket(&[0; 32], &service("1.2.3.4", 1), &ip("1.2.3.4"), &[0xff])
                .unwrap(),
            None
        );
    }
}

#[test]
fn chance_matches_formula_bit_exactly() {
    for oracle in Oracle::all() {
        for attempts in [-1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 1000, i32::MIN, i32::MAX] {
            for delta in [0i64, 599, 600, 601, -1, i64::MAX] {
                let mut want = 1.0f64;
                if delta < 600 {
                    want *= 0.01;
                }
                want *= 0.66f64.powf(f64::from(attempts.min(8)));
                let got = oracle.addrman_chance(delta, attempts).unwrap();
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "{} attempts {attempts} delta {delta}",
                    oracle.pin()
                );
            }
        }
        assert_eq!(oracle.addrman_chance(i64::MIN, 0).unwrap_err().kind, ErrorKind::Argument);
    }
}

#[test]
fn addrman_simple_replays() {
    // addrman_simple (addrman_tests.cpp:62-104), deterministic, check ratio 100 as the test's default.
    let config = AddrManConfig {
        check_ratio: 100,
        ..AddrManConfig::default()
    };
    let source = ip("252.2.2.2");
    let add = |addrs: Vec<Address>| AddrOp::Add {
        addrs,
        source: source.clone(),
        time_penalty: 0,
    };
    let size = AddrOp::Size {
        network: None,
        in_new: None,
    };
    let select = AddrOp::Select {
        new_only: false,
        network: None,
    };
    for oracle in Oracle::all() {
        let mut am = new_addrman(&oracle, &config);
        assert_eq!(run(&mut am, size.clone(), T), OpResult::Size(0));
        assert_eq!(selected(run(&mut am, select.clone(), T)), service("::", 0));
        assert_eq!(run(&mut am, add(vec![address("250.1.1.1", 8333)]), T), OpResult::Bool(true));
        assert_eq!(run(&mut am, size.clone(), T), OpResult::Size(1));
        assert_eq!(selected(run(&mut am, select.clone(), T)), service("250.1.1.1", 8333));
        assert_eq!(run(&mut am, add(vec![address("250.1.1.1", 8333)]), T), OpResult::Bool(false));
        assert_eq!(run(&mut am, size.clone(), T), OpResult::Size(1));
        assert_eq!(run(&mut am, add(vec![address("250.1.1.2", 8333)]), T), OpResult::Bool(true));

        let state = am.canonical().unwrap();
        assert_eq!(state.key, {
            let mut one = [0u8; 32];
            one[0] = 1;
            one
        });
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.random.len(), 2);
        assert_eq!(state.new.len(), 2);
        assert!(state.tried.is_empty());
        assert_eq!(state.network_counts, vec![(NET_IPV4, 2, 0)]);
    }
}

#[test]
fn addrman_evictionworks_replays() {
    // addrman_evictionworks (addrman_tests.cpp:898-955); Core's explicit times become mock times.
    let source = ip("252.2.2.2");
    let config = AddrManConfig {
        check_ratio: 100,
        ..AddrManConfig::default()
    };
    let collision = AddrOp::SelectTriedCollision;
    for oracle in Oracle::all() {
        let mut am = new_addrman(&oracle, &config);
        assert_eq!(selected(run(&mut am, collision.clone(), T)), service("::", 0));
        for i in 1..36 {
            let addr = address(&format!("250.1.1.{i}"), 0);
            let added = AddrOp::Add {
                addrs: vec![addr.clone()],
                source: source.clone(),
                time_penalty: 0,
            };
            assert_eq!(run(&mut am, added, T), OpResult::Bool(true));
            assert_eq!(run(&mut am, AddrOp::Good(addr.service), T), OpResult::Bool(true), "{i}");
        }
        let a36 = address("250.1.1.36", 0);
        let added = AddrOp::Add {
            addrs: vec![a36.clone()],
            source: source.clone(),
            time_penalty: 0,
        };
        assert_eq!(run(&mut am, added, T), OpResult::Bool(true));
        assert_eq!(run(&mut am, AddrOp::Good(a36.service.clone()), T), OpResult::Bool(false));
        let a19 = selected(run(&mut am, collision.clone(), T));
        assert_eq!(a19, service("250.1.1.19", 0));
        assert_eq!(run(&mut am, AddrOp::Good(a19.clone()), 1), OpResult::Bool(false));
        let attempt = AddrOp::Attempt {
            addr: a19.clone(),
            count_failure: false,
        };
        assert_eq!(run(&mut am, attempt, T - 61), OpResult::Done);
        assert_eq!(run(&mut am, AddrOp::ResolveCollisions, T), OpResult::Done);
        assert_eq!(selected(run(&mut am, collision.clone(), T)), service("::", 0));
        assert_eq!(run(&mut am, AddrOp::Good(a36.service.clone()), T), OpResult::Bool(false));
        assert_eq!(run(&mut am, AddrOp::Good(a19.clone()), T), OpResult::Bool(false));
        assert_eq!(selected(run(&mut am, collision.clone(), T)), a36.service);
        assert_eq!(run(&mut am, AddrOp::ResolveCollisions, T + 4 * 60 * 60), OpResult::Done);
        assert_eq!(selected(run(&mut am, collision.clone(), T)), service("::", 0));

        let state = am.canonical().unwrap();
        let in_tried =
            |s: &Service| state.entries.iter().find(|e| &e.addr.service == s).unwrap().in_tried;
        assert!(in_tried(&a19), "19 back in tried");
        assert!(!in_tried(&a36.service), "36 back in new");
        assert!(state.collisions.is_empty());
    }
}

#[test]
fn dump_and_load_round_trip() {
    let source = ip("252.2.2.2");
    for oracle in Oracle::all() {
        let mut am = new_addrman(&oracle, &AddrManConfig::default());
        let addrs: Vec<Address> =
            (1..=40).map(|i| address(&format!("250.{i}.1.1"), 9999)).collect();
        run(
            &mut am,
            AddrOp::Add {
                addrs,
                source: source.clone(),
                time_penalty: 0,
            },
            T,
        );
        let OpResult::File(file) = run(&mut am, AddrOp::Dump, T) else {
            panic!("dump")
        };
        // regtest message start, then the format byte V4_MULTIPORT and lowest compatible 32 + 4.
        assert_eq!(file[..6], [0xfc, 0xc1, 0xb7, 0xdc, 4, 36]);
        let checksum = sha256d::Hash::hash(&file[..file.len() - 32]).to_byte_array();
        assert_eq!(file[file.len() - 32..], checksum);

        let mut loaded = new_addrman(&oracle, &AddrManConfig::default());
        let load = |file: Vec<u8>| AddrOp::Load(file);
        assert!(matches!(
            run(&mut loaded, load(file.clone()), T),
            OpResult::Loaded {
                outcome: LoadOutcome::Ok,
                ..
            }
        ));
        // Serialize renumbers entries in mapInfo (unordered_map) order, so a reload need not
        // re-dump the same bytes; it keeps the same addresses in the same table cells.
        let cells = |s: &AddrManState| {
            let by_id = |id: i32| s.entries.iter().find(|e| e.id == id).unwrap();
            let mut cells: Vec<_> = s
                .new
                .iter()
                .chain(&s.tried)
                .map(|c| {
                    let e = by_id(c.id);
                    (c.bucket, c.position, e.in_tried, e.addr.clone(), e.source.clone())
                })
                .collect();
            cells.sort_by_key(|c| (c.0, c.1, c.2));
            cells
        };
        let (a, b) = (am.canonical().unwrap(), loaded.canonical().unwrap());
        assert_eq!(
            (a.key, a.n_new, a.n_tried, a.entries.len()),
            (b.key, b.n_new, b.n_tried, b.entries.len())
        );
        assert_eq!(cells(&a), cells(&b));
        let OpResult::File(redump) = run(&mut loaded, AddrOp::Dump, T) else {
            panic!("dump")
        };
        assert_eq!(redump.len(), file.len());

        // A truncated file and a bad checksum are fatal at startup; a file a newer format
        // made incompatible is moved to .bak. Each leaves an empty addrman.
        let mut truncated = file.clone();
        truncated.truncate(file.len() / 2);
        let mut bad_sum = file.clone();
        *bad_sum.last_mut().unwrap() ^= 1;
        let mut newer = file[..file.len() - 32].to_vec();
        newer[5] = 32 + 99;
        let sum = sha256d::Hash::hash(&newer).to_byte_array();
        newer.extend(sum);
        for (file, want) in [
            (truncated, LoadOutcome::Fatal),
            (bad_sum, LoadOutcome::Fatal),
            (newer, LoadOutcome::Backup),
        ] {
            let OpResult::Loaded {
                outcome,
                message,
            } = run(&mut loaded, load(file), T)
            else {
                panic!("load")
            };
            assert_eq!(outcome, want, "{message}");
            assert_eq!(
                run(
                    &mut loaded,
                    AddrOp::Size {
                        network: None,
                        in_new: None
                    },
                    T
                ),
                OpResult::Size(0)
            );
        }

        // Another network's message start is fatal too.
        let mut main = new_addrman(
            &oracle,
            &AddrManConfig {
                chain: Chain::main(),
                ..AddrManConfig::default()
            },
        );
        let OpResult::Loaded {
            outcome,
            ..
        } = run(&mut main, load(file), T)
        else {
            panic!("load")
        };
        assert_eq!(outcome, LoadOutcome::Fatal);
    }
}

#[test]
fn refuses_bad_calls() {
    for oracle in Oracle::all() {
        let bad = |config: AddrManConfig| oracle.addrman_new(&config).map(|a| a.is_some());
        assert_eq!(
            bad(AddrManConfig {
                check_ratio: -1,
                ..Default::default()
            })
            .unwrap_err()
            .kind,
            ErrorKind::Argument
        );
        assert!(
            !bad(AddrManConfig {
                asmap: vec![0xff],
                ..Default::default()
            })
            .unwrap(),
            "an insane asmap is a verdict"
        );
        let mut am = new_addrman(&oracle, &AddrManConfig::default());
        let err = |am: &mut AddrMan, op: &[u8], t| am.op_raw(op, t).unwrap_err().kind;
        assert_eq!(err(&mut am, &[], T), ErrorKind::Argument);
        assert_eq!(err(&mut am, &[42], T), ErrorKind::Argument);
        assert_eq!(err(&mut am, &[5], 0), ErrorKind::Argument);
        assert_eq!(err(&mut am, &[6, 2, 0xff], T), ErrorKind::Argument);
        assert_eq!(err(&mut am, &[6, 0, 7], T), ErrorKind::Argument);
        assert_eq!(err(&mut am, &[6, 0], T), ErrorKind::Decode);
        assert_eq!(err(&mut am, &[5, 0], T), ErrorKind::Trailing);
    }
}

/// A script that reaches every draw site: a self-seeding addrman (nKey and the RNG from the
/// seed), consistency checks every other operation, Add's 2^-refcount rule, Select, GetAddr's
/// shuffle and collision resolution. Returns every raw output and the final canonical state.
fn seeded_script(oracle: &Oracle, seed: u64) -> Vec<Vec<u8>> {
    let config = AddrManConfig {
        deterministic: false,
        rng_seed: seed,
        check_ratio: 2,
        ..AddrManConfig::default()
    };
    let mut am = new_addrman(oracle, &config);
    let mut outputs = Vec::new();
    let mut op =
        |am: &mut AddrMan, op: AddrOp, t: i64| outputs.push(am.op_raw(&op.encode(), t).unwrap());
    for round in 0..4u8 {
        for src in 0..8u8 {
            let addrs = (0..16u8)
                .map(|i| address(&format!("{}.{}.{}.1", 20 + i, src, round), 9999))
                .collect();
            op(
                &mut am,
                AddrOp::Add {
                    addrs,
                    source: ip(&format!("100.{src}.0.1")),
                    time_penalty: 7200,
                },
                T,
            );
        }
        for i in 0..16u8 {
            op(
                &mut am,
                AddrOp::Good(service(&format!("{}.{round}.{round}.1", 20 + i), 9999)),
                T + 60,
            );
        }
        op(
            &mut am,
            AddrOp::Select {
                new_only: false,
                network: None,
            },
            T + 120,
        );
        op(
            &mut am,
            AddrOp::Select {
                new_only: true,
                network: Some(NET_IPV4),
            },
            T + 120,
        );
        op(
            &mut am,
            AddrOp::GetAddr {
                max_addresses: 23,
                max_pct: 50,
                network: None,
                filtered: true,
            },
            T + 180,
        );
        op(&mut am, AddrOp::SelectTriedCollision, T + 240);
        op(&mut am, AddrOp::ResolveCollisions, T + 5 * 3600);
    }
    outputs.push(am.canonical_raw().unwrap());
    outputs
}

fn digest(outputs: &[Vec<u8>]) -> String {
    let mut all = Vec::new();
    for o in outputs {
        all.extend((o.len() as u64).to_le_bytes());
        all.extend(o);
    }
    sha256::Hash::hash(&all).to_string()
}

/// Printed by the child processes of `seeded_draws_replay_across_processes`.
#[test]
fn seeded_script_digest_child() {
    if std::env::var_os("O1C_DIGEST_CHILD").is_none() {
        return;
    }
    for oracle in Oracle::all() {
        println!("DIGEST {} {}", oracle.pin(), digest(&seeded_script(&oracle, 42)));
    }
}

#[test]
fn seeded_draws_replay_across_processes() {
    for oracle in Oracle::all() {
        let first = seeded_script(&oracle, 42);
        assert_eq!(first, seeded_script(&oracle, 42), "same seed, same process");
        assert_ne!(
            digest(&first),
            digest(&seeded_script(&oracle, 43)),
            "the seed reaches the draws"
        );

        // Every kind of record shows up: the self-seeding context, its seed and its keystream.
        let mut am = new_addrman(
            &oracle,
            &AddrManConfig {
                deterministic: false,
                rng_seed: 7,
                ..AddrManConfig::default()
            },
        );
        let out = am.op(&AddrOp::Load(Vec::new()), T).unwrap();
        assert!(
            matches!(
                out.draws[0],
                Draw::NewContext {
                    deterministic: false,
                    ..
                }
            ),
            "{:?}",
            out.draws
        );
        assert!(out.draws.iter().any(|d| matches!(d, Draw::RandBytes(b) if b.len() == 32)));
        assert!(
            out.draws
                .iter()
                .any(|d| matches!(d, Draw::Keystream { bytes, .. } if bytes.len() == 32))
        );
    }
    let want: Vec<String> = Oracle::all()
        .map(|o| format!("DIGEST {} {}", o.pin(), digest(&seeded_script(&o, 42))))
        .collect();
    for _ in 0..2 {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "seeded_script_digest_child", "--nocapture", "--test-threads=1"])
            .env("O1C_DIGEST_CHILD", "1")
            .output()
            .unwrap();
        assert!(child.status.success(), "{}", String::from_utf8_lossy(&child.stderr));
        let lines: Vec<String> = String::from_utf8_lossy(&child.stdout)
            .lines()
            // libtest prints the line after "test seeded_script_digest_child ... ".
            .filter_map(|l| l.find("DIGEST ").map(|at| l[at..].to_owned()))
            .collect();
        assert_eq!(lines, want, "a fresh process replays the draws");
    }
}

fn keystream_bytes(draws: &[Draw]) -> u64 {
    draws
        .iter()
        .map(|d| match d {
            Draw::Keystream {
                bytes,
                ..
            } => bytes.len() as u64,
            _ => 0,
        })
        .sum()
}

/// Many addresses from many groups, so `GetAddr` returns more than 4 KiB; and the keystream
/// bytes the adds logged.
fn crowded(oracle: &Oracle, seed: u64) -> (AddrMan, u64) {
    let mut am = new_addrman(
        oracle,
        &AddrManConfig {
            deterministic: false,
            rng_seed: seed,
            ..AddrManConfig::default()
        },
    );
    let mut logged = 0;
    for a in 10..60u8 {
        let addrs = (0..20u8).map(|b| address(&format!("{a}.{b}.1.1"), 9999)).collect();
        let source = ip(&format!("{a}.200.0.1"));
        logged += keystream_bytes(
            &am.op(
                &AddrOp::Add {
                    addrs,
                    source,
                    time_penalty: 0,
                },
                T,
            )
            .unwrap()
            .draws,
        );
    }
    (am, logged)
}

const GET_ALL: AddrOp = AddrOp::GetAddr {
    max_addresses: 0,
    max_pct: 0,
    network: None,
    filtered: false,
};

/// O1c ruling: a stateful call runs exactly once. An output too big for the first buffer is
/// collected by a second call that the shim answers without running the operation again.
#[test]
fn an_oversized_output_runs_the_operation_once() {
    for oracle in Oracle::all() {
        let (mut am, _) = crowded(&oracle, 9);
        let raw = am.op_raw(&GET_ALL.encode(), T + 60).unwrap();
        assert!(raw.len() > 4096, "{} bytes fit the first buffer", raw.len());
        // Every keystream byte the addrman's RNG produced is in a draw log, but for the 32 its
        // constructor draws for nKey (`insecure_rand.rand256()`, before any operation).
        let (mut am, mut logged) = crowded(&oracle, 9);
        for t in [T + 60, T + 120, T + 180] {
            let out = am.op(&GET_ALL, t).unwrap();
            let OpResult::Addrs(addrs) = &out.result else {
                panic!("addrs")
            };
            assert!(addrs.len() > 300);
            logged += keystream_bytes(&out.draws);
        }
        let state = am.canonical().unwrap();
        assert_eq!(state.rng_keystream_bytes, 32 + logged, "the counter equals the logged draws");

        // A forced retry (16-byte first buffer) and a twin read in one go agree, op by op, and
        // leave the same state.
        let (mut small, mut large) = (crowded(&oracle, 10).0, crowded(&oracle, 10).0);
        for t in [T + 60, T + 120] {
            let op = GET_ALL.encode();
            assert_eq!(
                small.op_raw_with_capacity(&op, t, 16).unwrap(),
                large.op_raw_with_capacity(&op, t, 1 << 20).unwrap()
            );
        }
        assert_eq!(small.canonical_raw().unwrap(), large.canonical_raw().unwrap());
    }
}
