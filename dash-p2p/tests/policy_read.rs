//! `policy::read` and `policy::read_headers2` give a handler's refusal where Core's handler
//! checks the payload before its read and scores, not a throw (SB6 r5 m6):
//! - a `headers`/`headers2` count over `GetHeadersLimit` is `Misbehaving(20, "headers message
//!   size = N")` and a return (v24.0.0-rc.3 `net_processing.cpp:5323-5327`), with the limit
//!   raised for `headers2` from `INCREASE_MAX_HEADERS2_VERSION` = 70235 (`version.h:50`,
//!   `net_processing.cpp:1210-1216`);
//! - a `qgetdata` over the request's 67 bytes is `PeerMisbehaving(100, "oversized qgetdata")`
//!   before the read (`llmq/net_quorum.cpp:90-93`).
//!
//! Each bound is tested on both sides.

use dash_p2p::policy::{self, ReadError, ReadPlan, Throw};
use dash_p2p::{CommandString, ProtocolVersion};
use dash_p2p_core::{Headers, Headers2, ProtoCtx, QuorumDataRequest};
use dash_pkc::bls::BlsCtx;
use dash_primitives::BlockHeader;
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Hashable};

const EOF: &str = "DataStream::read(): end of data: iostream error";

fn ctx(version: i32) -> ProtoCtx {
    ProtoCtx::new(ProtocolVersion(version), BlsCtx::BASIC)
}

fn compact(n: usize) -> Vec<u8> {
    let mut v = Vec::new();
    CompactSize::from(n).encode(&mut v);
    v
}

fn thrown<T: std::fmt::Debug>(r: Result<T, ReadError>) -> Throw {
    match r {
        Err(ReadError::Throw(t)) => t,
        r => panic!("not a throw: {r:?}"),
    }
}

/// A `headers` payload counting `count` and holding `n` headers (80 bytes and a tx count of 0).
fn headers(count: usize, n: usize) -> Vec<u8> {
    let mut p = compact(count);
    for i in 0..n {
        p.extend(0x2000_0000i32.to_le_bytes());
        p.extend([i as u8; 64]);
        p.extend(1_700_000_000u32.to_le_bytes());
        p.extend(0x1d00_ffffu32.to_le_bytes());
        p.extend((i as u32).to_le_bytes());
        p.push(0);
    }
    p
}

/// A `headers2` payload counting `count` and holding `n` headers: the first whole, the rest in
/// Core's commonest compressed form (version offset 1; previous hash, time and bits derived).
fn headers2(count: usize, n: usize) -> Vec<u8> {
    let mut p = compact(count);
    for i in 0..n {
        if i == 0 {
            p.push(0b0011_1000);
            p.extend(0x2000_0000i32.to_le_bytes());
            p.extend([0x44; 32]);
            p.extend([0x55; 32]);
            p.extend(1_700_000_000u32.to_le_bytes());
            p.extend(0x1d00_ffffu32.to_le_bytes());
        } else {
            p.push(0b0000_0001);
            p.extend([i as u8; 32]);
            p.extend(150i16.to_le_bytes());
        }
        p.extend((i as u32).to_le_bytes());
    }
    p
}

/// `read`'s result and the bytes it left.
fn read_at<T: dash_p2p_core::MsgDecode + 'static>(
    p: &[u8],
    ctx: &ProtoCtx,
) -> (Result<T, ReadError>, usize) {
    let mut cursor = p;
    let r = policy::read::<T>(&mut cursor, ctx);
    (r, cursor.len())
}

fn too_many(count: u64, max: u64) -> ReadError {
    ReadError::TooMany {
        count,
        max,
    }
}

#[test]
fn headers_limit_is_get_headers_limit() {
    for (version, compressed, limit) in [
        (70234, true, 2000),
        (70235, true, 8000),
        (70242, true, 8000),
        (70234, false, 2000),
        (70235, false, 2000),
        (70242, false, 2000),
    ] {
        assert_eq!(policy::headers_limit(ProtocolVersion(version), compressed), limit);
    }
    // The table's columns are the same function.
    let row = |name| policy::lookup(&CommandString::from_static(name)).unwrap();
    let ReadPlan::CountThenItems {
        limit,
    } = row("headers").read
    else {
        panic!("headers' read plan")
    };
    assert_eq!(limit, policy::headers_limit(ProtocolVersion(70242), false));
    let ReadPlan::CountThenItemsByVersion {
        limit,
        raised_limit,
        raised_from,
    } = row("headers2").read
    else {
        panic!("headers2's read plan")
    };
    for v in [raised_from - 1, raised_from, raised_from + 7] {
        let column = if v >= raised_from {
            raised_limit
        } else {
            limit
        };
        assert_eq!(column, policy::headers_limit(ProtocolVersion(v), true), "{v}");
    }
}

/// `headers`: 2000 headers read; a count of 2001 is Core's `Misbehaving(20)` and return, before
/// any header and whether or not the headers follow, at every version.
#[test]
fn headers_over_the_limit_score_and_do_not_throw() {
    for version in [70234, 70235, 70242] {
        let ctx = ctx(version);
        let (r, left) = read_at::<Headers>(&headers(2000, 2000), &ctx);
        assert_eq!((r.map(|h| h.headers.len()), left), (Ok(2000), 0), "{version}");
        for p in [headers(2001, 2001), headers(2001, 0)] {
            let (r, left) = read_at::<Headers>(&p, &ctx);
            let e = r.unwrap_err();
            assert_eq!((&e, left), (&too_many(2001, 2000), p.len()), "{version}");
            assert_eq!(e.misbehaving(), Some((20, "headers message size = 2001".to_owned())));
        }
        // A count within the limit that the bytes cannot fill still throws (Core's reads run
        // out of bytes).
        let (r, left) = read_at::<Headers>(&headers(2000, 3), &ctx);
        let t = thrown(r);
        assert_eq!((t.what.as_ref(), left), (EOF, headers(2000, 3).len()));
    }
}

/// `headers2` through both readers: the limit is 2000 below 70235 and 8000 from 70235 on
/// (`>=`, so 70235 itself takes the raised limit).
#[test]
fn headers2_limit_by_version_on_both_readers() {
    for (version, max) in [(70234, 2000usize), (70235, 8000), (70242, 8000)] {
        let ctx = ctx(version);
        for count in [2000, 2001, max, max + 1] {
            let p = headers2(count, count);
            let want = if count > max {
                Err(too_many(count as u64, max as u64))
            } else {
                Ok(count)
            };
            // Read whole, or left as it was with no header hashed.
            let (left_want, calls_want) = match want {
                Ok(_) => (0, count - 1),
                Err(_) => (p.len(), 0),
            };
            let (r, left) = read_at::<Headers2>(&p, &ctx);
            let r = r.map(|h| h.block_headers().len());
            assert_eq!((r, left), (want.clone(), left_want), "read::<Headers2> {version} {count}");

            let mut cursor = &p[..];
            let mut calls = 0;
            let r = policy::read_headers2(&mut cursor, &ctx, |h: &BlockHeader| {
                calls += 1;
                h.hash()
            });
            assert_eq!(
                (r.map(|h| h.len()), calls, cursor.len()),
                (want, calls_want, left_want),
                "read_headers2 {version} {count}"
            );
        }
        // Over the limit with no header following: refused on the count.
        let p = compact(max + 1);
        let (r, left) = read_at::<Headers2>(&p, &ctx);
        assert_eq!((r.map(|_| ()), left), (Err(too_many(max as u64 + 1, max as u64)), p.len()));
        let e = policy::read_headers2(&mut &p[..], &ctx, |h| h.hash()).unwrap_err();
        assert_eq!(e.misbehaving(), Some((20, format!("headers message size = {}", max + 1))));
    }
}

/// `qgetdata`: 67 bytes read; 68 and more are Core's `PeerMisbehaving(100, "oversized
/// qgetdata")` before the read; fewer than 67 run out of bytes.
#[test]
fn qgetdata_over_67_bytes_scores_before_the_read() {
    let ctx = ctx(70242);
    let request = |len: usize| {
        let mut p = vec![1u8];
        p.extend([0x11; 32]);
        p.extend(3u16.to_le_bytes());
        p.extend([0x22; 32]);
        p.resize(len, 0xff);
        p
    };
    let (r, left) = read_at::<QuorumDataRequest>(&request(67), &ctx);
    let q = r.unwrap();
    assert_eq!((q.data_mask, left), (3, 0));
    for len in [68, 69, 1000] {
        let p = request(len);
        let (r, left) = read_at::<QuorumDataRequest>(&p, &ctx);
        let e = r.unwrap_err();
        assert_eq!(
            (&e, left),
            (
                &ReadError::Oversized {
                    size: len,
                    max: 67
                },
                len
            )
        );
        assert_eq!(e.misbehaving(), Some((100, "oversized qgetdata".to_owned())));
    }
    for len in [0, 66] {
        let p = request(67)[..len].to_vec();
        let (r, left) = read_at::<QuorumDataRequest>(&p, &ctx);
        let t = thrown(r);
        assert_eq!((t.what.as_ref(), left), (EOF, len));
    }
}

/// A throw is no handler refusal.
#[test]
fn a_throw_has_no_score() {
    let (r, _) = read_at::<Headers>(&[], &ctx(70242));
    let e = r.unwrap_err();
    assert!(matches!(e, ReadError::Throw(_)));
    assert_eq!(e.misbehaving(), None);
}
