//! `headers2`'s version list is most-recently-used: a version taken by offset moves to the front
//! (Core's `MarkVersionAsMostRecent`, v24.0.0-rc.3 `primitives/block.cpp:37-43`, called from
//! `Uncompress` at `:107-117`), so offset 2 followed by offset 1 takes the same version twice
//! (SB6 r5 m12). Both the hash-once reader and the decode boundary read it so.

use dash_p2p::CommandString;
use dash_p2p::policy;
use dash_p2p_core::{P2pMsg, ProtoCtx, ProtocolVersion};
use dash_pkc::bls::BlsCtx;
use dash_types::Hashable;

/// Versions 100, 200 on the wire, then offsets 2, 1, 2: Core's `Compress` of the versions
/// 100, 200, 100, 100, 200 (the list goes [100], [200, 100], [100, 200], [100, 200],
/// [200, 100]). Without the move to the front, offset 1 would take 200 and the last offset 2
/// would take 100.
fn payload() -> Vec<u8> {
    let mut p = vec![5u8];
    // First: everything on the wire.
    p.push(0b0011_1000);
    p.extend(100i32.to_le_bytes());
    p.extend([0x44; 64]);
    p.extend(1_700_000_000u32.to_le_bytes());
    p.extend(0x1d00_ffffu32.to_le_bytes());
    p.extend(0u32.to_le_bytes());
    // Second: a new version on the wire; the previous hash, time and bits derived.
    p.push(0b0000_0000);
    p.extend(200i32.to_le_bytes());
    p.extend([0x01; 32]);
    p.extend(150i16.to_le_bytes());
    p.extend(1u32.to_le_bytes());
    // Then offsets 2, 1, 2.
    for (i, offset) in [2u8, 1, 2].into_iter().enumerate() {
        p.push(offset);
        p.extend([i as u8 + 2; 32]);
        p.extend(150i16.to_le_bytes());
        p.extend((i as u32 + 2).to_le_bytes());
    }
    p
}

#[test]
fn a_version_taken_by_offset_moves_to_the_front() {
    let ctx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);
    let p = payload();
    let mut cursor = &p[..];
    let headers = policy::read_headers2(&mut cursor, &ctx, |h| h.hash()).unwrap();
    assert!(cursor.is_empty());
    let versions: Vec<i32> = headers.iter().map(|h| h.version).collect();
    assert_eq!(versions, [100, 200, 100, 100, 200]);
    assert!(headers.windows(2).all(|w| w[1].prev_hash == w[0].hash()));

    let d = policy::decode(&CommandString::HEADERS2, &p, &ctx).unwrap();
    assert_eq!(d.consumed, p.len());
    let P2pMsg::Headers2(h) = d.msg else {
        panic!("headers2")
    };
    assert!(h.block_headers().eq(headers.iter()));
}
