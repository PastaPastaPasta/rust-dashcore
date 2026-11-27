//! The `headers2` bridge before and after it left base-sdk's `decode_with_hasher` (T12b §4 item
//! 5): the old path, kept here as it was, and the new one give the same result for every corpus
//! case. Deleted in T12b S5 with the other shadow tests, when the re-pin drops
//! `decode_with_hasher`.

use dash_consensus::Header;
use dash_p2p_core::{Headers2, ProtoCtx, ProtocolVersion};
use dash_pkc::bls::BlsCtx;
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;

use super::messages::{headers2_bytes_to_base, headers2_bytes_to_consensus, headers2_max};
use super::uncompress::tests::cases;
use super::{decode_all, BridgeError};

fn old_ctx() -> ProtoCtx {
    ProtoCtx::new(ProtocolVersion::V24_0, BlsCtx::BASIC)
}

/// The old `headers2_to_consensus`, from the bytes on.
fn old_to_consensus(bytes: &[u8]) -> Result<Vec<Header>, BridgeError> {
    let mut hashed = Vec::new();
    let headers2 = decode_all("headers2", bytes, |cursor| {
        Headers2::decode_with_hasher(cursor, &old_ctx(), |header: &BlockHeader| {
            let header = Header::new(*header);
            hashed.push(header);
            header.hash()
        })
    })?;
    let mut hashed = hashed.into_iter().peekable();
    Ok(headers2
        .block_headers()
        .map(|raw| {
            hashed.next_if(|header| header.raw() == raw).unwrap_or_else(|| Header::new(*raw))
        })
        .collect())
}

/// The old `headers2_to_base`, from the bytes on.
fn old_to_base(bytes: &[u8], base: &[(BlockHeader, BlockHash)]) -> Result<Headers2, BridgeError> {
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
    let headers2 = decode_all("headers2", bytes, |cursor| {
        Headers2::decode_with_hasher(cursor, &old_ctx(), hash)
    })?;
    if let Some(index) =
        headers2.block_headers().zip(base).position(|(restored, (header, _))| restored != header)
    {
        return Err(BridgeError::Headers2Mismatch {
            index,
        });
    }
    Ok(headers2)
}

#[test]
fn the_batch_bound_is_the_old_one() {
    assert_eq!(headers2_max(), Headers2::max_count(ProtocolVersion::V24_0));
}

/// Both paths over every corpus case, into `dash-consensus` and into base-sdk with the hashes
/// the crossing into `dash-consensus` gave and with header 0's hash wrong.
#[test]
fn old_and_new_headers2_bridges_agree() {
    let cases = cases();
    let mut accepted = 0;
    for bytes in &cases {
        let consensus = headers2_bytes_to_consensus(bytes);
        assert_eq!(consensus, old_to_consensus(bytes), "{bytes:02x?}");
        let mut base: Vec<_> = consensus
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|header| (*header.raw(), header.hash()))
            .collect();
        assert_eq!(headers2_bytes_to_base(bytes, &base), old_to_base(bytes, &base), "{bytes:02x?}");
        if let Some((_, hash)) = base.first_mut() {
            *hash = BlockHash::from_lendian([7; 32]);
            assert_eq!(
                headers2_bytes_to_base(bytes, &base),
                old_to_base(bytes, &base),
                "{bytes:02x?}"
            );
        }
        accepted += usize::from(consensus.is_ok());
    }
    assert!(accepted > 0 && accepted < cases.len());
}
