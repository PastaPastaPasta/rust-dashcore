//! The pure half of the `headers`/`headers2` branch (P2a §3.4.3, §3.4.9, §3.4.10): Core's read,
//! its `Uncompress`, and `CheckHeadersAreContinuous`, each hashing exactly where Core does.
//!
//! Every hash goes through a caller's `hash` function, so tests count the calls (the G1
//! witnesses). A header hashed by `Uncompress` is memoised as a [`Header`] and reused by the
//! continuity check, so no header is hashed twice on the actor (DEC-51a; D17).

use dash_consensus::Header;
/// Core's `GetHeadersLimit(pfrom, compressed)`: SB6's, the limit its `headers2` reader refuses at.
pub use dash_p2p::policy::headers_limit;
use dash_p2p::policy::{self, ReadError, Throw};
use dash_p2p_core::{P2pDecodeError, ProtoCtx};
use dash_primitives::BlockHeader;
use dash_types::codec::{BaseCodec, DecodeError};
use dash_types::{CompactSize, Numeric};

/// Core's `MAX_BLOCKS_TO_ANNOUNCE`.
pub const MAX_BLOCKS_TO_ANNOUNCE: usize = 8;
/// Core's `MAX_UNCONNECTING_HEADERS`.
pub const MAX_UNCONNECTING_HEADERS: i32 = 10;

fn throw(e: DecodeError) -> Throw {
    Throw::from_decode(&P2pDecodeError::from(e))
}

/// Core's `unsigned int nCount = ReadCompactSize(vRecv)`.
pub fn read_count(data: &mut &[u8]) -> Result<u64, Throw> {
    CompactSize::decode_size(data).map(|c| c.to_base()).map_err(throw)
}

/// A decoded message: the headers in message order, and the [`Header`]s `Uncompress` already
/// hashed (`memo[i]` is `headers[i]`'s, set when header `i + 1` took its previous hash).
#[derive(Debug, Default)]
pub struct Decoded {
    /// The headers, compressed fields restored.
    pub headers: Vec<BlockHeader>,
    /// The memoised hashes, one slot per header.
    pub memo: Vec<Option<Header>>,
}

impl Decoded {
    /// Appends a header with an empty memo slot.
    fn push(&mut self, h: BlockHeader) {
        self.headers.push(h);
        self.memo.push(None);
    }
}

/// Core's `headers` loop after the count check: each header, then a transaction count read and
/// ignored. Hashes nothing.
// Core: net_processing.cpp:5329-5334
pub fn read_headers(data: &mut &[u8], count: u64) -> Result<Decoded, Throw> {
    let mut out = Decoded::default();
    for _ in 0..count {
        out.push(BlockHeader::decode(data).map_err(throw)?);
        CompactSize::decode_size(data).map_err(throw)?;
    }
    Ok(out)
}

/// Core's `headers2` read, through SB6's [`policy::read_headers2`] (one pass of Core's
/// `Uncompress`, with Core's leniency; D48). `payload` starts at the count, which the caller has
/// already checked against [`headers_limit`]. `hash` runs exactly where `Uncompress` calls
/// `GetHash`: once per header after the first whose previous hash is left off, for the headers
/// read before a failure. Each hashed header is memoised for [`check_continuous`].
// Core: net_processing.cpp:5335-5343; primitives/block.cpp:95-134 Uncompress
pub fn read_headers2(
    payload: &mut &[u8],
    ctx: &ProtoCtx,
    mut hash: impl FnMut(&BlockHeader) -> Header,
) -> Result<Decoded, ReadError> {
    let mut hashed: Vec<Header> = Vec::new();
    let headers = policy::read_headers2(payload, ctx, |h| {
        let header = hash(h);
        let id = header.hash();
        hashed.push(header);
        id
    })?;
    // The hook ran in order, once for each header `k` whose previous hash it gave, on header
    // `k - 1`: pair each call with the first such `k` left.
    let mut memo = vec![None; headers.len()];
    let mut calls = hashed.into_iter().peekable();
    for (i, pair) in headers.windows(2).enumerate() {
        if calls.peek().is_some_and(|c| *c.raw() == pair[0] && c.hash() == pair[1].prev_hash) {
            memo[i] = calls.next();
        }
    }
    Ok(Decoded {
        headers,
        memo,
    })
}

/// Core's `CheckHeadersAreContinuous`, hashing as Core's loop does: compare a header's previous
/// hash against the last hash, return on a mismatch, then hash the header. A memoised hash is
/// reused. `Err(k)` is a mismatch at position `k`, after `k` headers were hashed.
// Core: net_processing.cpp:3167-3177 PeerManagerImpl::CheckHeadersAreContinuous
pub fn check_continuous(
    decoded: Decoded,
    mut hash: impl FnMut(&BlockHeader) -> Header,
) -> Result<Vec<Header>, usize> {
    let mut out: Vec<Header> = Vec::with_capacity(decoded.headers.len());
    for (k, (raw, memo)) in decoded.headers.iter().zip(decoded.memo).enumerate() {
        if let Some(last) = out.last()
            && raw.prev_hash != last.hash()
        {
            return Err(k);
        }
        out.push(memo.unwrap_or_else(|| hash(raw)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use dash_consensus::BlsCtx;
    use dash_p2p_core::{Headers2, ProtocolVersion};
    use dash_primitives::{BlockHash, MerkleRoot};

    use super::*;

    fn chain(len: usize) -> Vec<BlockHeader> {
        let mut out: Vec<BlockHeader> = Vec::new();
        for i in 0..len {
            let prev_hash = out.last().map(|h| Header::new(*h).hash()).unwrap_or_default();
            out.push(BlockHeader {
                version: [0x2000_0000, 0x2000_0004][i % 2],
                prev_hash,
                merkle_root: MerkleRoot::decode(&mut &[i as u8; 32][..]).expect("32 bytes"),
                time: 1_700_000_000 + (i as u32) * 150,
                bits: if i % 3 == 0 {
                    0x1b04_0000
                } else {
                    0x1b03_ffff
                },
                nonce: i as u32,
            });
        }
        out
    }

    fn wire2(headers: &[BlockHeader]) -> Vec<u8> {
        let mut buf = Vec::new();
        Headers2::reply(headers).encode(&mut buf);
        buf
    }

    /// A v24 peer's context.
    const V24: ProtoCtx = ProtoCtx::new(ProtocolVersion(70242), BlsCtx::BASIC);

    /// Decodes `bytes` as `headers2`, returning the result and the nonces hashed, in order.
    fn decode2(bytes: &[u8]) -> (Result<Decoded, ReadError>, Vec<u32>) {
        let mut nonces = Vec::new();
        let r = read_headers2(&mut &bytes[..], &V24, |h| {
            nonces.push(h.nonce);
            Header::new(*h)
        });
        (r, nonces)
    }

    #[test]
    fn a_connecting_headers2_batch_hashes_each_header_once() {
        let headers = chain(10);
        let (decoded, hashed) = decode2(&wire2(&headers));
        let decoded = decoded.expect("decodes");
        assert_eq!(decoded.headers, headers);
        // Uncompress hashed headers 0..9 for 1..10.
        assert_eq!(hashed, (0..9).collect::<Vec<_>>());
        let mut more = Vec::new();
        let out = check_continuous(decoded, |h| {
            more.push(h.nonce);
            Header::new(*h)
        })
        .expect("continuous");
        // Only the last is left: count = Uncompress hashes + 1 (P2a §3.4.9 witnesses).
        assert_eq!(more, vec![9]);
        assert_eq!(out.iter().map(Header::hash).collect::<Vec<_>>(), {
            headers.iter().map(|h| Header::new(*h).hash()).collect::<Vec<_>>()
        });
    }

    #[test]
    fn a_plain_batch_hashes_up_to_the_mismatch() {
        for k in [1usize, 1000, 1999] {
            let mut headers = chain(2000);
            headers[k].prev_hash = BlockHash::default();
            let mut buf = Vec::new();
            dash_p2p_core::Headers {
                headers: headers.clone(),
            }
            .encode(&mut buf);
            let mut data = &buf[..];
            let count = read_count(&mut data).expect("count");
            let decoded = read_headers(&mut data, count).expect("decodes");
            let mut n = 0;
            let r = check_continuous(decoded, |h| {
                n += 1;
                Header::new(*h)
            });
            assert_eq!(r.err(), Some(k));
            // A mismatch at position k costs k hashes (Core's "k−1" counts from 1).
            assert_eq!(n, k);
        }
    }

    #[test]
    fn explicit_prev_hashes_are_not_hashed_by_the_decoder() {
        let headers = chain(3);
        let mut buf = vec![3u8];
        for h in &headers {
            // Flags: every field on the wire.
            buf.push(0b0011_1000);
            h.version.encode(&mut buf);
            h.prev_hash.encode(&mut buf);
            h.merkle_root.encode(&mut buf);
            h.time.encode(&mut buf);
            h.bits.encode(&mut buf);
            h.nonce.encode(&mut buf);
        }
        let (decoded, hashed) = decode2(&buf);
        assert_eq!(decoded.expect("decodes").headers, headers);
        assert!(hashed.is_empty());
    }

    #[test]
    fn core_leniency_a_short_first_header_and_an_offset_past_the_list() {
        let headers = chain(2);
        let mut buf = vec![2u8];
        // First header: version offset 1 (left off), no prev hash, delta time, no bits.
        buf.push(0b0000_0001);
        headers[0].merkle_root.encode(&mut buf);
        7i16.encode(&mut buf);
        headers[0].nonce.encode(&mut buf);
        // Second: offset 5 past a one-entry list, everything else compressed.
        buf.push(0b0000_0101);
        headers[1].merkle_root.encode(&mut buf);
        150i16.encode(&mut buf);
        headers[1].nonce.encode(&mut buf);
        let (decoded, hashed) = decode2(&buf);
        let d = decoded.expect("Core accepts both");
        assert_eq!(d.headers[0].version, 0);
        assert_eq!(d.headers[0].time, 0);
        assert_eq!(d.headers[0].prev_hash, BlockHash::default());
        assert_eq!(d.headers[1].version, 0);
        assert_eq!(d.headers[1].time, 150);
        assert_eq!(d.headers[1].prev_hash, Header::new(d.headers[0]).hash());
        assert_eq!(hashed, vec![headers[0].nonce]);
    }

    #[test]
    fn a_truncated_message_throws_after_the_hashes_before_it() {
        let headers = chain(4);
        let bytes = wire2(&headers);
        let (r, hashed) = decode2(&bytes[..bytes.len() - 2]);
        assert!(r.is_err());
        assert_eq!(hashed, vec![0, 1]);
    }

    /// `headers` as a `getheaders2` reply, one encoded header per entry (flags byte first).
    fn wire2_parts(headers: &[BlockHeader]) -> Vec<Vec<u8>> {
        Headers2::reply(headers)
            .headers()
            .iter()
            .map(|c| {
                let mut buf = Vec::new();
                c.encode(&mut buf);
                buf
            })
            .collect()
    }

    /// The decode and the continuity check of one batch: the headers, and the hashes both made.
    fn hashes_for(bytes: &[u8]) -> (Vec<BlockHeader>, usize) {
        let mut n = 0;
        let mut count_hash = |h: &BlockHeader| {
            n += 1;
            Header::new(*h)
        };
        let decoded = read_headers2(&mut &bytes[..], &V24, &mut count_hash).expect("Core reads it");
        let headers = decoded.headers.clone();
        check_continuous(decoded, &mut count_hash).expect("continuous");
        (headers, n)
    }

    /// DEC-51a, D48: a full `headers2` batch costs one hash per header (Core's 7999 `Uncompress`
    /// hashes through SB6's hook, then the last header's in `CheckHeadersAreContinuous`). That
    /// holds for a batch with a header only Core's lenient `Uncompress` takes too (at header 0,
    /// or at header 7990 after 7989 hashes), which a strict reader with a fallback would hash
    /// again (about 16,000).
    #[test]
    fn an_8000_header_batch_hashes_8000_times_on_either_path() {
        let headers = chain(8000);
        let mut parts = wire2_parts(&headers);
        let join = |parts: &[Vec<u8>]| {
            let mut buf = Vec::new();
            CompactSize::from(parts.len()).encode(&mut buf);
            parts.iter().for_each(|p| buf.extend(p));
            buf
        };

        let (got, n) = hashes_for(&join(&parts));
        assert_eq!(got, headers);
        assert_eq!(n, 8000);

        // Header 7990's version offset past the two-entry list (BSX D-07), which a strict
        // decoder refuses after hashing the headers before it.
        let mut late = parts.clone();
        assert_ne!(late[7990][0] & 0b111, 0, "header 7990's version is in the list");
        late[7990][0] |= 0b111;
        let lenient = join(&late);
        assert!(Headers2::decode(&mut &lenient[..]).is_err(), "strict refuses");
        let (got, n) = hashes_for(&lenient);
        assert_eq!(got[7990].version, 0);
        assert_eq!(n, 8000);

        // A first header with its prev hash left off (zero, as `chain`'s is): refused before
        // any hash.
        let first = &mut parts[0];
        assert_eq!(first[0], 0b0011_1000, "a whole first header");
        first[0] &= !0b0000_1000;
        first.drain(5..37);
        let lenient = join(&parts);
        assert!(Headers2::decode(&mut &lenient[..]).is_err(), "strict refuses");
        let (got, n) = hashes_for(&lenient);
        assert_eq!(got[0], headers[0]);
        assert_eq!(n, 8000);
    }
}
