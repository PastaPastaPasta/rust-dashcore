//! The P2P messages dash-spv hands to the shared crates, between dashcore's types and base-sdk's.
//!
//! `headers` maps field by field ([`super::headers`]). `headers2` crosses to base-sdk with the
//! hashes dash-spv's decompressor computed, so base-sdk's decoder hashes nothing, and into
//! `dash-consensus` by a decompression that hashes each header there once. The rest cross as
//! Core's serialization, decoded whole by the other side; a `mnlistdiff` and the diffs inside a
//! `qrinfo` take the current layout (protocol 70230 and later, `MnListFormat::CURRENT`), the one
//! dashcore reads and writes.

use dash_consensus::Header;
use dash_p2p_core::{
    CFHeaders, CFilter, ClSig, Headers, Headers2, IsdLock, MnListDiff, ProtoCtx, ProtocolVersion,
    QrInfo,
};
use dash_pkc::bls::BlsCtx;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;
use dashcore::block::Header as CoreHeader;
use dashcore::consensus::serialize;
use dashcore::network::message_filter;
use dashcore::network::message_headers2::Headers2Message;
use dashcore::network::message_qrinfo::QRInfo;
use dashcore::network::message_sml;
use dashcore::{ChainLock, InstantLock};

use super::headers::{block_hash_to_base, header_from_base, header_to_base};
use super::{decode_all, from_base, to_base, BridgeError};

/// A `headers` message as base-sdk's [`Headers`], field for field.
pub fn headers_to_base(headers: &[CoreHeader]) -> Headers {
    Headers {
        headers: headers.iter().map(header_to_base).collect(),
    }
}

/// A `headers` message as dashcore's header list, field for field.
pub fn headers_from_base(headers: &Headers) -> Vec<CoreHeader> {
    headers.headers.iter().map(header_from_base).collect()
}

/// A `headers2` message as base-sdk's [`Headers2`], given the headers dash-spv decompressed from
/// it with their hashes, in order (`CompressionState::process_headers_with_hashes`). base-sdk's
/// decoder takes each previous hash it restores from those, so nothing is hashed again, and the
/// headers it restores must equal `decompressed`'s. The hashes themselves are trusted: a list
/// whose hashes disagree with the message fails, but one the decompressor got wrong consistently
/// cannot be told apart without hashing.
///
/// # Errors
///
/// [`BridgeError::HashCount`] if `decompressed` does not hold one entry per header,
/// [`BridgeError::ToBase`] if base-sdk refuses the message, and
/// [`BridgeError::Headers2Mismatch`] if the headers it restores are not `decompressed`'s.
pub fn headers2_to_base(
    message: &Headers2Message,
    decompressed: &[(CoreHeader, dashcore::BlockHash)],
) -> Result<Headers2, BridgeError> {
    if message.headers.len() != decompressed.len() {
        return Err(BridgeError::HashCount {
            headers: message.headers.len(),
            hashes: decompressed.len(),
        });
    }
    let base: Vec<(BlockHeader, BlockHash)> = decompressed
        .iter()
        .map(|(header, hash)| (header_to_base(header), block_hash_to_base(hash)))
        .collect();
    // base-sdk asks, in order, for the hash of the header before each one whose previous hash
    // the message leaves off. A header that is not in the batch gets a hash no header has, and
    // the comparison below reports it.
    let mut next = 0;
    let hash = |header: &BlockHeader| {
        while let Some((candidate, hash)) = base.get(next) {
            next += 1;
            if candidate == header {
                return *hash;
            }
        }
        BlockHash::from_lendian([0; 32])
    };
    let headers2 = decode_all("headers2", &serialize(message), |cursor| {
        Headers2::decode_with_hasher(cursor, &headers2_ctx(), hash)
    })?;
    if let Some(index) =
        headers2.block_headers().zip(&base).position(|(restored, (header, _))| restored != header)
    {
        return Err(BridgeError::Headers2Mismatch {
            index,
        });
    }
    Ok(headers2)
}

/// A `headers2` message's headers in `dash-consensus`, each hashed there once (DEC-224).
/// base-sdk decompresses the message; where it needs a header's hash to restore the next
/// header's previous hash, the header is hashed by `dash-consensus` and that header kept, and
/// every other header is hashed after. dash-spv's headers and hashes are
/// [`super::headers::header_from_consensus`] of the result.
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses the message.
pub fn headers2_to_consensus(message: &Headers2Message) -> Result<Vec<Header>, BridgeError> {
    let mut hashed = Vec::new();
    let headers2 = decode_all("headers2", &serialize(message), |cursor| {
        Headers2::decode_with_hasher(cursor, &headers2_ctx(), |header: &BlockHeader| {
            let header = Header::new(*header);
            hashed.push(header);
            header.hash()
        })
    })?;
    // base-sdk asks for hashes in order, each header's at most once.
    let mut hashed = hashed.into_iter().peekable();
    Ok(headers2
        .block_headers()
        .map(|raw| {
            hashed.next_if(|header| header.raw() == raw).unwrap_or_else(|| Header::new(*raw))
        })
        .collect())
}

/// The newest protocol: the version only sets the largest `headers2` batch base-sdk accepts
/// (8,000, as dashcore's decoder does), and a peer's reply is never larger than what it
/// negotiated.
fn headers2_ctx() -> ProtoCtx {
    ProtoCtx::new(ProtocolVersion::V24_0, BlsCtx::BASIC)
}

/// A `headers2` message as dashcore's [`Headers2Message`].
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses the message base-sdk wrote.
pub fn headers2_from_base(headers2: &Headers2) -> Result<Headers2Message, BridgeError> {
    from_base("headers2", headers2)
}

/// A `cfilter` message as base-sdk's [`CFilter`].
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses it.
pub fn cfilter_to_base(cfilter: &message_filter::CFilter) -> Result<CFilter, BridgeError> {
    to_base("cfilter", cfilter)
}

/// A `cfilter` message as dashcore's [`message_filter::CFilter`].
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses it.
pub fn cfilter_from_base(cfilter: &CFilter) -> Result<message_filter::CFilter, BridgeError> {
    from_base("cfilter", cfilter)
}

/// A `cfheaders` message as base-sdk's [`CFHeaders`].
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses it.
pub fn cfheaders_to_base(cfheaders: &message_filter::CFHeaders) -> Result<CFHeaders, BridgeError> {
    to_base("cfheaders", cfheaders)
}

/// A `cfheaders` message as dashcore's [`message_filter::CFHeaders`].
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses it.
pub fn cfheaders_from_base(
    cfheaders: &CFHeaders,
) -> Result<message_filter::CFHeaders, BridgeError> {
    from_base("cfheaders", cfheaders)
}

/// A `mnlistdiff` message as base-sdk's [`MnListDiff`].
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses it.
pub fn mnlistdiff_to_base(diff: &message_sml::MnListDiff) -> Result<MnListDiff, BridgeError> {
    to_base("mnlistdiff", diff)
}

/// A `mnlistdiff` message as dashcore's [`message_sml::MnListDiff`].
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses it.
pub fn mnlistdiff_from_base(diff: &MnListDiff) -> Result<message_sml::MnListDiff, BridgeError> {
    from_base("mnlistdiff", diff)
}

/// A `qrinfo` message as base-sdk's [`QrInfo`].
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses it.
pub fn qrinfo_to_base(qrinfo: &QRInfo) -> Result<QrInfo, BridgeError> {
    to_base("qrinfo", qrinfo)
}

/// A `qrinfo` message as dashcore's [`QRInfo`].
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses it.
pub fn qrinfo_from_base(qrinfo: &QrInfo) -> Result<QRInfo, BridgeError> {
    from_base("qrinfo", qrinfo)
}

/// A `clsig` message as base-sdk's [`ClSig`].
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses it.
pub fn clsig_to_base(chain_lock: &ChainLock) -> Result<ClSig, BridgeError> {
    to_base("clsig", chain_lock)
}

/// A `clsig` message as dashcore's [`ChainLock`].
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses it.
pub fn clsig_from_base(clsig: &ClSig) -> Result<ChainLock, BridgeError> {
    from_base("clsig", clsig)
}

/// An `isdlock` message as base-sdk's [`IsdLock`].
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses it.
pub fn isdlock_to_base(instant_lock: &InstantLock) -> Result<IsdLock, BridgeError> {
    to_base("isdlock", instant_lock)
}

/// An `isdlock` message as dashcore's [`InstantLock`].
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses it.
pub fn isdlock_from_base(isdlock: &IsdLock) -> Result<InstantLock, BridgeError> {
    from_base("isdlock", isdlock)
}

#[cfg(test)]
pub(super) mod tests {
    use dash_types::codec::BaseCodec as _;
    use dashcore::block::Version;
    use dashcore::consensus::deserialize;
    use dashcore::hashes::Hash;
    use dashcore::network::message_headers2::{CompressionFlags, CompressionState};
    use dashcore::{CompactTarget, TxMerkleNode};

    use super::*;

    /// Core's mainnet `headers2` reply and dash-spv's decompression of it, with its hashes.
    fn mainnet_headers2() -> (Headers2Message, Vec<(CoreHeader, dashcore::BlockHash)>) {
        let message: Headers2Message =
            deserialize(include_bytes!("../../tests/data/bridge/mainnet/headers2-2080001.bin"))
                .expect("dashcore decodes the capture");
        let decompressed = CompressionState::new()
            .process_headers_with_hashes(&message.headers)
            .expect("dashcore decompresses Core's headers2");
        assert!(message.headers[1].prev_blockhash.is_none(), "header 1 leaves its prev hash off");
        (message, decompressed)
    }

    /// The crossing takes header 0's hash from the list, not from X11: with a stand-in hash, and
    /// header 1 naming it, the restored header 1 carries the stand-in. A crossing that hashed
    /// header 0 itself would restore the real hash and fail.
    #[test]
    fn headers2_takes_the_given_hashes() {
        let (message, mut decompressed) = mainnet_headers2();
        let stand_in = dashcore::BlockHash::from_byte_array([7; 32]);
        decompressed[0].1 = stand_in;
        decompressed[1].0.prev_blockhash = stand_in;
        let base = headers2_to_base(&message, &decompressed).expect("crosses");
        let restored = base.block_headers().nth(1).expect("header 1");
        assert_eq!(restored.prev_hash, block_hash_to_base(&stand_in));
    }

    /// A hash list that disagrees with the message fails instead of crossing.
    #[test]
    fn headers2_refuses_hashes_that_disagree() {
        let (message, mut decompressed) = mainnet_headers2();
        decompressed[0].1 = dashcore::BlockHash::from_byte_array([7; 32]);
        assert_eq!(
            headers2_to_base(&message, &decompressed),
            Err(BridgeError::Headers2Mismatch {
                index: 1
            })
        );
        decompressed.pop();
        assert_eq!(
            headers2_to_base(&message, &decompressed),
            Err(BridgeError::HashCount {
                headers: 8000,
                hashes: 7999
            })
        );
    }

    /// Every header a `headers2` crossing into `dash-consensus` returns is the decompressed one,
    /// with its X11.
    #[test]
    fn headers2_into_consensus_restores_each_header_with_its_hash() {
        let (message, decompressed) = mainnet_headers2();
        let consensus = headers2_to_consensus(&message).expect("crosses");
        assert_eq!(consensus.len(), decompressed.len());
        for (header, (core, hash)) in consensus.iter().zip(&decompressed) {
            assert_eq!(*header.raw(), header_to_base(core));
            assert_eq!(header.hash(), block_hash_to_base(hash));
        }
    }

    /// A chain of synthetic headers, linked by their real X11 hashes, whose `headers2` encoding
    /// holds what the captured replies do not: the cached versions at offsets 6 and 7 (the
    /// version list's last two positions) and a negative time offset, with a full timestamp and a
    /// changed `nBits` after them, and a header in the middle that carries its previous hash
    /// although it follows its parent. dashcore's encoder writes it: Core's never writes a
    /// negative offset (it subtracts in `uint32_t`, so a step back takes a full timestamp), but
    /// Core's decoder reads one (`int16_t`), as it reads a prev hash it could have restored, so a
    /// peer may send either.
    fn synthetic_headers2() -> (Vec<CoreHeader>, Vec<u8>) {
        let version = |n: i32| Version::from_consensus(0x2000_0000 | n);
        let mut header = CoreHeader {
            version: version(0),
            prev_blockhash: dashcore::BlockHash::from_byte_array([1; 32]),
            merkle_root: TxMerkleNode::from_byte_array([2; 32]),
            time: 1_700_000_000,
            bits: CompactTarget::from_consensus(0x1b04_864c),
            nonce: 0,
        };
        let mut headers = vec![header];
        // Seven new versions fill the list (newest first: 7..1); 2 is then at position 5
        // (offset 6), and after it 1 is at position 6 (offset 7), its last.
        let steps: [(i32, i64, Option<u32>); 11] = [
            (1, 150, None),
            (2, 150, None),
            (3, 150, None),
            (4, 150, None),
            (5, 150, None),
            (6, 150, None),
            (7, 150, None),
            (2, 150, None),
            (1, -600, None),
            (1, 40_000, None),
            (1, 150, Some(0x1b04_8000)),
        ];
        for (n, (v, dt, bits)) in steps.into_iter().enumerate() {
            header = CoreHeader {
                version: version(v),
                prev_blockhash: header.block_hash(),
                merkle_root: TxMerkleNode::from_byte_array([n as u8 + 3; 32]),
                time: u32::try_from(i64::from(header.time) + dt).expect("in range"),
                bits: bits.map_or(header.bits, CompactTarget::from_consensus),
                nonce: n as u32 + 1,
            };
            headers.push(header);
        }
        let mut state = CompressionState::new();
        let mut message = Headers2Message {
            headers: headers.iter().map(|header| state.compress(header)).collect(),
        };
        with_prev_hash(&mut message, &headers, 5);
        (headers, serialize(&message))
    }

    /// Header `index` of `message` carries its previous hash, which the decoder could restore.
    pub(in crate::bridge) fn with_prev_hash(
        message: &mut Headers2Message,
        headers: &[CoreHeader],
        index: usize,
    ) {
        let compressed = &mut message.headers[index];
        assert!(compressed.prev_blockhash.is_none());
        compressed.prev_blockhash = Some(headers[index].prev_blockhash);
        compressed.flags.0 |= CompressionFlags::PREV_BLOCK_HASH;
    }

    /// The synthetic vector, byte-exact through dashcore and base-sdk, and into `dash-consensus`
    /// with every header's real X11.
    #[test]
    fn a_synthetic_headers2_vector_crosses_byte_exact() {
        let (headers, bytes) = synthetic_headers2();
        let message: Headers2Message = deserialize(&bytes).expect("dashcore decodes it");
        assert_eq!(serialize(&message), bytes, "dashcore writes other bytes");
        let offsets: Vec<u8> = message.headers.iter().map(|h| h.flags.version_offset()).collect();
        assert_eq!(offsets[8..10], [6, 7], "cached versions at the list's last two positions");
        assert_eq!(message.headers[9].time_offset, Some(-600), "a negative time offset");
        assert!(message.headers[10].time_full.is_some() && message.headers[11].bits.is_some());
        assert!(message.headers[5].prev_blockhash.is_some(), "a prev hash it could restore");

        let decompressed = CompressionState::new()
            .process_headers_with_hashes(&message.headers)
            .expect("dashcore decompresses it");
        let expected: Vec<_> = headers.iter().map(|h| (*h, h.block_hash())).collect();
        assert_eq!(decompressed, expected);

        let base = headers2_to_base(&message, &decompressed).expect("crosses to base-sdk");
        let mut written = Vec::new();
        base.encode(&mut written);
        assert_eq!(written, bytes, "base-sdk writes other bytes");
        let back = headers2_from_base(&base).expect("crosses back");
        assert_eq!(serialize(&back), bytes);

        let consensus = headers2_to_consensus(&message).expect("crosses to dash-consensus");
        assert_eq!(consensus.len(), headers.len());
        for (header, (core, hash)) in consensus.iter().zip(&expected) {
            assert_eq!(*header.raw(), header_to_base(core));
            assert_eq!(header.hash(), block_hash_to_base(hash));
        }
    }
}
