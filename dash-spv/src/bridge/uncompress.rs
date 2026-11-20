//! A `headers2` payload read as Core's handler reads it: a copy of `dash_p2p::policy`'s
//! `read_headers2`, which dash-spv takes no dependency on (T12b §7 Q4). The tests pin the two
//! equal. The count is refused as the bridge's base-sdk decoder refused it before T12b, which
//! the bridge's errors have always said.

use std::collections::VecDeque;
use std::fmt;

use dash_primitives::{BlockHash, BlockHeader};
use dash_types::codec::{BaseCodec, DecodeError};
use dash_types::CompactSize;

/// `CompressedHeaderBitField`'s version offset: 0 when the version is on the wire.
const VERSION_OFFSET: u8 = 0b0000_0111;
/// `CompressedHeaderBitField` flags: the previous block hash, the timestamp and `nBits` are on
/// the wire.
pub(super) const PREV_HASH: u8 = 0b0000_1000;
const TIMESTAMP: u8 = 0b0001_0000;
const NBITS: u8 = 0b0010_0000;
/// The `last_unique_versions` bound.
const MAX_VERSIONS: usize = 7;

/// Why a `headers2` payload did not read, in the words of base-sdk's `P2pDecodeError`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Headers2Error {
    /// The count is over the batch bound; no header was read.
    TooMany {
        count: u64,
        max: usize,
    },
    /// A read failed.
    Decode(DecodeError),
}

impl From<DecodeError> for Headers2Error {
    fn from(e: DecodeError) -> Self {
        Self::Decode(e)
    }
}

impl fmt::Display for Headers2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooMany {
                count,
                max,
            } => write!(f, "headers2 lists {count} entries, max {max}"),
            Self::Decode(e) => write!(f, "{e}"),
        }
    }
}

/// Reads a `headers2` payload from the front of `data`: a count of at most `max` and no more
/// than the bytes left, then that many entries, each restored against the one before. `hash` is
/// called on the header before each entry that leaves its previous block hash off, once the
/// entry is read, in order: where Core's `Uncompress` hashes.
// Core: net_processing.cpp:5323-5343; primitives/block.cpp:37-55, :95-134 (Uncompress)
pub(super) fn read_headers2(
    data: &mut &[u8],
    max: usize,
    mut hash: impl FnMut(&BlockHeader) -> BlockHash,
) -> Result<Vec<BlockHeader>, Headers2Error> {
    let count = CompactSize::decode(data)?;
    if u64::from(count) > max as u64 {
        return Err(Headers2Error::TooMany {
            count: count.into(),
            max,
        });
    }
    let count = count.into_len(data.len())?;
    let mut versions: VecDeque<i32> = VecDeque::with_capacity(MAX_VERSIONS + 1);
    // Core: SaveVersionAsMostRecent
    fn save(versions: &mut VecDeque<i32>, v: i32) {
        versions.push_front(v);
        versions.truncate(MAX_VERSIONS);
    }
    let mut headers: Vec<BlockHeader> = Vec::with_capacity(count);
    for _ in 0..count {
        // The entry's wire fields, the others at the default header's (zero).
        let flags = u8::decode(data)?;
        let zero = BlockHeader::default();
        let mut version = if flags & VERSION_OFFSET == 0 {
            i32::decode(data)?
        } else {
            zero.version()
        };
        let mut prev_hash = if flags & PREV_HASH != 0 {
            BlockHash::decode(data)?
        } else {
            zero.prev_hash()
        };
        let merkle_root = BaseCodec::decode(data)?;
        let (mut time, time_offset) = if flags & TIMESTAMP != 0 {
            (u32::decode(data)?, 0)
        } else {
            (zero.time(), i16::decode(data)?)
        };
        let mut bits = if flags & NBITS != 0 {
            u32::decode(data)?
        } else {
            zero.bits()
        };
        let nonce = u32::decode(data)?;
        match headers.last() {
            None => save(&mut versions, version),
            Some(last) => {
                let offset = usize::from(flags & VERSION_OFFSET);
                if offset == 0 {
                    save(&mut versions, version);
                } else if let Some(v) = versions.remove(offset - 1) {
                    version = v;
                    versions.push_front(v);
                }
                if flags & PREV_HASH == 0 {
                    prev_hash = hash(last);
                }
                if flags & TIMESTAMP == 0 {
                    // `uint32_t + int16_t`: modulo 2^32.
                    time = last.time().wrapping_add(i32::from(time_offset) as u32);
                }
                if flags & NBITS == 0 {
                    bits = last.bits();
                }
            }
        }
        headers.push(BlockHeader::new(version, prev_hash, merkle_root, time, bits, nonce));
    }
    Ok(headers)
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::BTreeSet;

    use dash_p2p::policy::{self, ReadError};
    use dash_p2p::read::PeerCtx;
    use dash_p2p_core::{Headers2, ProtocolVersion};
    use dash_pkc::bls::BlsCtx;
    use dashcore::hashes::sha256;

    use super::*;

    const CORPORA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../test-vectors/p2p-decode");

    fn unhex(hex: &str) -> Option<Vec<u8>> {
        hex.len().is_multiple_of(2).then_some(())?;
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect()
    }

    /// Every `raw` payload in a corpus file: a hex string, or a list of hex strings joined. It
    /// doesn't expand `{"b": .., "n": ..}` runs; no `headers2` row uses them, and one that did
    /// would find no payload and panic in [`corpus_payloads`].
    fn raw_payloads(source: &str) -> Vec<Vec<u8>> {
        let text = std::fs::read_to_string(format!("{CORPORA}/{source}")).expect("corpus file");
        text.split("\"raw\":")
            .skip(1)
            .filter_map(|rest| {
                let rest = rest.trim_start();
                let value = if rest.starts_with('[') {
                    &rest[..rest.find(']')?]
                } else {
                    let end = rest[1..].find('"')? + 2;
                    &rest[..end]
                };
                unhex(&value.split('"').skip(1).step_by(2).collect::<String>())
            })
            .collect()
    }

    /// Every `headers2` payload `corpus_cases.tsv` lists, a long one read from its source file.
    fn corpus_payloads() -> Vec<Vec<u8>> {
        let manifest = std::fs::read_to_string(format!("{CORPORA}/corpus_cases.tsv")).unwrap();
        manifest
            .lines()
            .filter(|line| !line.starts_with('#'))
            .filter_map(|line| {
                let [source, "headers2", key] = line.split('|').collect::<Vec<_>>()[..] else {
                    return None;
                };
                let Some(long) = key.strip_prefix("sha256:") else {
                    return Some(unhex(key).expect("hex payload"));
                };
                let (digest, len) = long.split_once(':').unwrap();
                let found = raw_payloads(source).into_iter().find(|bytes| {
                    bytes.len().to_string() == len
                        && sha256::Hash::hash(bytes).to_string() == digest
                });
                Some(found.unwrap_or_else(|| panic!("no payload in {source} for {key}")))
            })
            .collect()
    }

    /// The corpus payloads, each also cut short, with a byte after it, and with its count one
    /// more, at the bound and one over it.
    pub(in crate::bridge) fn cases() -> Vec<Vec<u8>> {
        let payloads = corpus_payloads();
        assert_eq!(payloads.len(), 80, "corpus_cases.tsv's headers2 rows");
        let mut cases = BTreeSet::new();
        for payload in payloads {
            let len = payload.len();
            let cuts: Vec<usize> = if len <= 400 {
                (0..len).collect()
            } else {
                vec![1, 40, len / 2, len - 1]
            };
            cases.extend(cuts.into_iter().map(|cut| payload[..cut].to_vec()));
            cases.insert([&payload[..], &[0]].concat());
            let mut body = &payload[..];
            if let Ok(count) = CompactSize::decode(&mut body) {
                for count in [
                    u64::from(count) + 1,
                    Headers2::MAX_COUNT as u64,
                    Headers2::MAX_COUNT as u64 + 1,
                ] {
                    let mut over = Vec::new();
                    CompactSize::from(count).encode(&mut over);
                    over.extend_from_slice(body);
                    cases.insert(over);
                }
            }
            cases.insert(payload);
        }
        cases.into_iter().collect()
    }

    /// The copy reads every case as `dash_p2p::policy::read_headers2` does: the same verdict and,
    /// on success, the same headers, bytes read and hash calls.
    #[test]
    fn the_copy_reads_as_dash_p2p_does() {
        let ctx = PeerCtx::new(ProtocolVersion::CURRENT, BlsCtx::BASIC);
        let max = crate::bridge::messages::headers2_max();
        assert_eq!(
            max,
            policy::headers_limit(ctx.version(), true),
            "the bound the bridge reads with"
        );
        let cases = cases();
        let mut accepted = 0;
        for bytes in &cases {
            let (mut ours, mut theirs) = (&bytes[..], &bytes[..]);
            let (mut our_hashes, mut their_hashes) = (Vec::new(), Vec::new());
            let our = read_headers2(&mut ours, max, |h| {
                our_hashes.push(*h);
                dash_types::Hashable::hash(h)
            });
            let their = policy::read_headers2(&mut theirs, &ctx, |h| {
                their_hashes.push(*h);
                dash_types::Hashable::hash(h)
            });
            match (our, their) {
                (Ok(our), Ok(their)) => {
                    assert_eq!(our, their, "{bytes:02x?}");
                    assert_eq!(ours.len(), theirs.len(), "{bytes:02x?}");
                    assert_eq!(our_hashes, their_hashes, "{bytes:02x?}");
                    accepted += 1;
                }
                (
                    Err(Headers2Error::TooMany {
                        ..
                    }),
                    Err(ReadError::TooMany {
                        ..
                    }),
                ) => {}
                (Err(Headers2Error::Decode(_)), Err(ReadError::Throw(_))) => {}
                (our, their) => panic!("{bytes:02x?}: {our:?} against {their:?}"),
            }
        }
        assert!(accepted > 0 && accepted < cases.len());
    }
}
