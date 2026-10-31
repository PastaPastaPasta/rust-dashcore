//! [`Header`] and [`Block`]: a header bound to its X11 hash, and a block with its merkle roots
//! computed once (CS0 §6.3).

use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
use dash_types::Numeric as _;

use super::serialize::{compact_size_len, header_bytes};
use super::tx::TxRef;
use crate::hash;
use crate::merkle::compute_merkle_root;

/// An immutable block header bound to its X11 hash. Copying it copies the pair.
///
/// [`Header::new`] is the only way to make one, so the hash is always the X11 of the header's own
/// bytes: no caller can pair a header with a hash it supplies (DEC-224). Consumers never hash a
/// header again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    raw: BlockHeader,
    hash: BlockHash,
}

impl Header {
    /// Computes the header's hash: X11 of Core's 80-byte serialization.
    // Core: primitives/block.cpp:13 CBlockHeader::GetHash
    pub fn new(raw: BlockHeader) -> Header {
        let hash = BlockHash::from_lendian(hash::x11(&header_bytes(&raw)));
        Header {
            raw,
            hash,
        }
    }

    /// base-sdk's header.
    pub fn raw(&self) -> &BlockHeader {
        &self.raw
    }

    /// `GetHash()`.
    pub fn hash(&self) -> BlockHash {
        self.hash
    }

    pub fn version(&self) -> i32 {
        self.raw.version
    }

    pub fn prev_hash(&self) -> BlockHash {
        self.raw.prev_hash
    }

    pub fn merkle_root(&self) -> MerkleRoot {
        self.raw.merkle_root
    }

    pub fn time(&self) -> u32 {
        self.raw.time
    }

    pub fn bits(&self) -> u32 {
        self.raw.bits
    }

    pub fn nonce(&self) -> u32 {
        self.raw.nonce
    }
}

/// A block: a [`Header`] and shared transactions, with its size and merkle root computed once.
///
/// The merkle root's leaves are the txids, so a version-2+ asset unlock contributes its stable
/// txid (CS0 §5.1).
pub struct Block {
    header: Header,
    txs: Vec<TxRef>,
    size: u32,
    merkle: (MerkleRoot, bool),
}

impl Block {
    /// A block from parts (the miner, tests). The header is already hashed.
    pub fn new(header: Header, txs: Vec<TxRef>) -> Block {
        let tx_bytes: u64 = txs.iter().map(|tx| u64::from(tx.size())).sum();
        let size = 80 + compact_size_len(txs.len() as u64) as u64 + tx_bytes;
        let merkle = merkle(txs.iter().map(|tx| tx.txid()));
        Block {
            header,
            txs,
            size: u32::try_from(size).unwrap_or(u32::MAX),
            merkle,
        }
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// `GetHash()`.
    pub fn hash(&self) -> BlockHash {
        self.header.hash()
    }

    pub fn txs(&self) -> &[TxRef] {
        &self.txs
    }

    /// `GetSerializeSize`: the size of Core's serialization.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// `BlockMerkleRoot(block, &mutated)`: the root and the mutation flag.
    // Core: consensus/merkle.cpp:65 BlockMerkleRoot (leaves are GetHash())
    pub fn merkle(&self) -> (MerkleRoot, bool) {
        self.merkle
    }
}

fn merkle(leaves: impl Iterator<Item = dash_primitives::TxHash>) -> (MerkleRoot, bool) {
    let (root, mutated) = compute_merkle_root(leaves.map(|h| *h.as_bytes()).collect());
    (MerkleRoot::from_lendian(root), mutated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::x11_calls;
    use crate::primitives::{core_serialize, decode_header};

    // Stand-ins with the signatures CH1 and CS3 take (CS0 deviation 7).
    fn process_headers(headers: Vec<Header>) -> Vec<BlockHash> {
        headers.iter().map(check_block_header).collect()
    }

    fn check_block_header(header: &Header) -> BlockHash {
        header.hash()
    }

    /// From decode to the header checks, each header is hashed exactly once (CS0 §6.3).
    #[test]
    fn header_hand_off_hashes_once() {
        let wire: Vec<u8> = (0..2000u32)
            .flat_map(|nonce| {
                core_serialize(&BlockHeader {
                    nonce,
                    ..BlockHeader::default()
                })
            })
            .collect();
        let before = x11_calls::count();
        let mut cursor = &wire[..];
        let mut headers = Vec::new();
        while !cursor.is_empty() {
            headers.push(decode_header(&mut cursor).expect("decodes").into_value());
        }
        let hashes = process_headers(headers);
        assert_eq!(hashes.len(), 2000);
        assert_eq!(x11_calls::count() - before, 2000);
    }
}
