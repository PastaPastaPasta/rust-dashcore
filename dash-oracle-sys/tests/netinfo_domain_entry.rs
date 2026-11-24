//! The Domain first-entry vectors (R7; SW-V24 rc.4 audit §4.3, DEC-456 (2), DEC-461) and rc.4's
//! purpose order (CE4-01), as Core's `ExtNetInfo` decode answers them through
//! `oracle_netinfo_validate` (a decode failure is `ErrorKind::Decode`). Each value is one Core
//! P2P group of two entries, the second the valid service 1.1.1.1:9999 where the vector has one.
//! dash-dml's codec tests (`a_bad_domain_body_fails_at_the_first_entry` and the two after it)
//! hold the same vectors with the first entry's offset. Every expectation is Core's source
//! (`netinfo.h` rc.4:243-263, 419-437; `netinfo.cpp` rc.4:24, 90-113, 585-623), none was read off
//! the oracle.

use dash_oracle_sys::{Chain, ErrorKind, NetInfoKind, NetInfoStatus, Oracle, Pin};

const SERVICE: [u8; 9] = [0x01, 0x01, 0x04, 1, 1, 1, 1, 0x27, 0x0f];

fn compact(n: u64) -> Vec<u8> {
    match n {
        0..0xfd => vec![n as u8],
        0xfd..=0xffff => [vec![0xfd], (n as u16).to_le_bytes().to_vec()].concat(),
        _ => unreachable!("not needed here"),
    }
}

fn domain(claimed: u64, body: &[u8]) -> Vec<u8> {
    [&[2u8][..], &compact(claimed), body].concat()
}

fn full_domain(name: &str) -> Vec<u8> {
    domain(name.len() as u64, &[name.as_bytes(), &443u16.to_be_bytes()].concat())
}

fn core_p2p(entries: &[Vec<u8>]) -> Vec<u8> {
    [&[1u8, 1, 0, entries.len() as u8][..], &entries.concat()].concat()
}

#[test]
fn domain_first_entry_vectors() {
    let oracle = Oracle::new(Pin::V24_0);
    let chain = Chain::main();
    let svc = SERVICE.to_vec();
    let verdict = |bytes: &[u8]| oracle.netinfo_validate(&chain, NetInfoKind::Ext, bytes);
    // D1-D4: the first entry's body fails the decode.
    let rejected = [
        ("D1", core_p2p(&[domain(254, &[&[b'a'; 254][..], &[1, 0xbb]].concat()), svc.clone()])),
        ("D2", core_p2p(&[domain(0xffff, b"abc")])),
        ("D3", core_p2p(&[domain(20, &[b'a'; 10])])),
        ("D4", core_p2p(&[domain(11, &[&b"example.com"[..], &[1]].concat())])),
    ];
    for (name, bytes) in rejected {
        assert_eq!(verdict(&bytes).map_err(|e| e.kind), Err(ErrorKind::Decode), "{name}");
    }
    // D5 ("a..b", an empty label) and D6 (an unknown type) read as the invalid entry, which
    // Validate() refuses (Malformed: `bad-protx-netinfo-bad`); the control D5b's "a.b" is a valid
    // domain, which Core P2P does not take (BadInput: `bad-protx-netinfo-entry`).
    let decoded = [
        ("D5", core_p2p(&[full_domain("a..b"), svc.clone()]), NetInfoStatus::Malformed),
        ("D5b", core_p2p(&[full_domain("a.b"), svc.clone()]), NetInfoStatus::BadInput),
        ("D6", core_p2p(&[vec![0x07], svc.clone()]), NetInfoStatus::Malformed),
    ];
    for (name, bytes, status) in decoded {
        assert_eq!(verdict(&bytes).map_err(|e| e.kind), Ok(status), "{name}");
    }
}

/// CE4-01: a repeated or descending purpose fails the decode at rc.4 (rc.3 read the first
/// first-wins and the second in map order); ascending purposes read.
#[test]
fn purposes_must_ascend() {
    let oracle = Oracle::new(Pin::V24_0);
    let chain = Chain::main();
    let svc = SERVICE.to_vec();
    let p2p: Vec<u8> = (1..=5u8).flat_map(|i| [0x01, 0x01, 0x04, 1, 1, 2, i, 0x66, 0xd0]).collect();
    let verdict =
        |bytes: &[u8]| oracle.netinfo_validate(&chain, NetInfoKind::Ext, bytes).map_err(|e| e.kind);
    let repeated = [&[1u8, 2, 0, 1][..], &svc, &[0, 1], &svc].concat();
    let descending = [&[1u8, 2, 1, 5][..], &p2p, &[0, 1], &svc].concat();
    let unknown = [&[1u8, 2, 0, 1][..], &svc, &[3, 1], &svc].concat();
    for (name, bytes) in [("repeated", repeated), ("descending", descending), ("unknown", unknown)]
    {
        assert_eq!(verdict(&bytes), Err(ErrorKind::Decode), "{name}");
    }
    let ascending = [&[1u8, 2, 0, 1][..], &svc, &[1, 5], &p2p].concat();
    assert!(verdict(&ascending).is_ok());
}
