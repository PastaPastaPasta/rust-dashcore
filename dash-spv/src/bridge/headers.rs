//! Block headers and blocks, hashed once (SB0 §5.5, DEC-51a, DEC-224).
//!
//! `dash-consensus` computes the hash of every header it holds: [`Header::new`] is its only
//! constructor, so a hash there is always the X11 of the header's own bytes. A header dash-spv
//! receives therefore crosses unhashed ([`header_to_consensus`], [`block_to_consensus`],
//! [`super::messages::headers2_to_consensus`]), is hashed there once, and dash-spv takes its
//! value and hash from the result ([`header_from_consensus`], [`block_from_consensus`]). A
//! header that already comes with a hash ([`HashedBlockHeader`], [`HashedBlock`]: dash-spv's own
//! hashing, storage, any decoder) is hashed again on the way in, and refused if the two differ.

use std::sync::Arc;

use dash_consensus::{core_serialize, decode_tx_exact, Block, Header};
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
use dash_types::Numeric as _;
use dashcore::block::{Header as CoreHeader, Version};
use dashcore::consensus::serialize;
use dashcore::hashes::Hash;
use dashcore::Block as CoreBlock;
use dashcore::{CompactTarget, TxMerkleNode};

use super::{decode_dashcore, BridgeError};
use crate::types::{HashedBlock, HashedBlockHeader};

/// base-sdk's block hash: the same 32 bytes, in Core's order.
pub fn block_hash_to_base(hash: &dashcore::BlockHash) -> BlockHash {
    BlockHash::from_lendian(hash.to_byte_array())
}

/// dashcore's block hash: the same 32 bytes, in Core's order.
pub fn block_hash_from_base(hash: &BlockHash) -> dashcore::BlockHash {
    dashcore::BlockHash::from_byte_array(hash.to_lendian())
}

/// base-sdk's header, field for field. Nothing is hashed.
pub fn header_to_base(header: &CoreHeader) -> BlockHeader {
    BlockHeader {
        version: header.version.to_consensus(),
        prev_hash: block_hash_to_base(&header.prev_blockhash),
        merkle_root: MerkleRoot::from_lendian(header.merkle_root.to_byte_array()),
        time: header.time,
        bits: header.bits.to_consensus(),
        nonce: header.nonce,
    }
}

/// dashcore's header, field for field. Nothing is hashed.
pub fn header_from_base(header: &BlockHeader) -> CoreHeader {
    CoreHeader {
        version: Version::from_consensus(header.version),
        prev_blockhash: block_hash_from_base(&header.prev_hash),
        merkle_root: TxMerkleNode::from_byte_array(header.merkle_root.to_lendian()),
        time: header.time,
        bits: CompactTarget::from_consensus(header.bits),
        nonce: header.nonce,
    }
}

/// `dash-consensus`'s header, hashed there: the one X11 this header gets on the way in.
pub fn header_to_consensus(header: &CoreHeader) -> Header {
    Header::new(header_to_base(header))
}

/// [`header_to_consensus`] over a batch, in order.
pub fn headers_to_consensus(headers: &[CoreHeader]) -> Vec<Header> {
    headers.iter().map(header_to_consensus).collect()
}

/// `dash-consensus`'s header for one dash-spv paired with a hash. The header is hashed again,
/// since nothing proves where the pair came from (storage and its decoder read the hash apart
/// from the header), and the pair is refused unless the two hashes agree.
///
/// # Errors
///
/// [`BridgeError::WrongHash`] if the hash is not the header's X11.
pub fn hashed_header_to_consensus(header: &HashedBlockHeader) -> Result<Header, BridgeError> {
    let consensus = header_to_consensus(header.header());
    same_hash("header", consensus.hash(), header.hash())?;
    Ok(consensus)
}

fn same_hash(
    kind: &'static str,
    computed: BlockHash,
    given: &dashcore::BlockHash,
) -> Result<(), BridgeError> {
    if computed == block_hash_to_base(given) {
        Ok(())
    } else {
        Err(BridgeError::WrongHash {
            kind,
        })
    }
}

/// dash-spv's header for a `dash-consensus` one: it keeps the header's hash.
pub fn header_from_consensus(header: &Header) -> HashedBlockHeader {
    HashedBlockHeader::with_trusted_hash(
        header_from_base(header.raw()),
        block_hash_from_base(&header.hash()),
    )
}

/// [`header_from_consensus`] over a batch, in order.
pub fn headers_from_consensus(headers: &[Header]) -> Vec<HashedBlockHeader> {
    headers.iter().map(header_from_consensus).collect()
}

/// `dash-consensus`'s block: the header is hashed there (once), and each transaction crosses as
/// Core's serialization.
///
/// # Errors
///
/// [`BridgeError::ToBase`] if `dash-consensus` refuses a transaction dashcore wrote.
pub fn block_to_consensus(block: &CoreBlock) -> Result<Block, BridgeError> {
    block_with_header(block, header_to_consensus(&block.header))
}

fn block_with_header(block: &CoreBlock, header: Header) -> Result<Block, BridgeError> {
    let txs = block
        .txdata
        .iter()
        .map(|tx| {
            decode_tx_exact(&serialize(tx)).map(|tx| Arc::new(tx.into_value())).map_err(|e| {
                BridgeError::ToBase {
                    kind: "block",
                    reason: e.to_string(),
                }
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(Block::new(header, txs))
}

/// [`block_to_consensus`] for a block dash-spv paired with a hash, refused unless that hash is
/// the header's X11 (as [`hashed_header_to_consensus`]).
///
/// # Errors
///
/// [`BridgeError::WrongHash`] if the hash is not the header's X11, and
/// [`block_to_consensus`]'s errors.
pub fn hashed_block_to_consensus(block: &HashedBlock) -> Result<Block, BridgeError> {
    let header = header_to_consensus(&block.block().header);
    same_hash("block", header.hash(), block.hash())?;
    block_with_header(block.block(), header)
}

/// dash-spv's block for a `dash-consensus` one, which keeps the block's hash.
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses the block `dash-consensus` wrote.
pub fn block_from_consensus(block: &Block) -> Result<HashedBlock, BridgeError> {
    let core = decode_dashcore("block", &core_serialize(block))?;
    Ok(HashedBlock::with_trusted_hash(core, block_hash_from_base(&block.hash())))
}

#[cfg(test)]
mod tests {
    use dashcore::consensus::{deserialize, serialize};

    use super::*;

    /// A real mainnet block, whose hash meets its target.
    fn mainnet_block() -> CoreBlock {
        deserialize(include_bytes!("../../tests/data/bridge/mainnet/block-2089979.bin"))
            .expect("dashcore decodes the block")
    }

    /// The forgery DEC-224 rules out: a header whose own X11 misses its target, paired with a
    /// hash that meets it (the real block's), through the public decoders that read the hash
    /// apart from the header (storage's). The crossing hashes the header itself and refuses the
    /// pair, in release builds too.
    #[test]
    fn a_forged_header_hash_pair_is_refused() {
        let block = mainnet_block();
        let real = block.block_hash();
        let mut forged = block.header;
        forged.nonce = forged.nonce.wrapping_add(1);
        let target = forged.target();
        assert!(target.is_met_by(real) && !target.is_met_by(forged.block_hash()));

        let mut bytes = serialize(&forged);
        bytes.extend_from_slice(&serialize(&real));
        let stored: HashedBlockHeader = deserialize(&bytes).expect("a stored header decodes");
        assert_eq!(stored.hash(), &real, "the decoder keeps the hash it read");
        assert_eq!(
            hashed_header_to_consensus(&stored),
            Err(BridgeError::WrongHash {
                kind: "header"
            })
        );
        // What `dash-consensus` holds for those bytes, PoW's input, is their own hash.
        assert_ne!(header_to_consensus(&forged).hash(), block_hash_to_base(&real));

        let honest = HashedBlockHeader::from(&block.header);
        let consensus = hashed_header_to_consensus(&honest).expect("the real pair crosses");
        assert_eq!(consensus.hash(), block_hash_to_base(&real));
    }

    /// The same forgery for a block: storage's decoder reads the hash first, then the block.
    #[test]
    fn a_forged_block_hash_pair_is_refused() {
        let mut block = mainnet_block();
        let real = block.block_hash();
        block.header.nonce = block.header.nonce.wrapping_add(1);

        let mut bytes = serialize(&real);
        bytes.extend_from_slice(&serialize(&block));
        let stored: HashedBlock = deserialize(&bytes).expect("a stored block decodes");
        assert_eq!(stored.hash(), &real);
        assert!(matches!(
            hashed_block_to_consensus(&stored),
            Err(BridgeError::WrongHash {
                kind: "block"
            })
        ));
        let consensus = block_to_consensus(&block).expect("the block itself crosses");
        assert_ne!(consensus.hash(), block_hash_to_base(&real));
    }
}
