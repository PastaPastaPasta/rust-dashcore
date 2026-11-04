//! The toy's synthetic blocks (design §6.2, Q8): the one place the toy makes block bytes.
//!
//! A block is a zero-work header whose previous-block hash is its parent's id, and one coinbase
//! whose `OP_RETURN` output is padded (with non-zero bytes derived from its height and branch)
//! so the block has exactly the configured size (1,024 bytes in the PR gate, 900 in S6). The bytes are decoded by CS0's decoder, so the block files hold
//! real `CoreBytes`. A block's id is `SHA256d` of its bytes; ids differ between branches because
//! the header names the branch.

use bitcoin_hashes::{Hash as _, sha256d};

use crate::flatfile::{CoreBytes, CoreUndoBytes};
use dash_kv::faulty::Hash;

/// Header, tx count, version, input (count, prevout, 2-byte scriptSig, sequence), output count,
/// value, lock time: everything but the output script and its size.
const FIXED: usize = 80 + 1 + 4 + 1 + 36 + 3 + 4 + 1 + 8 + 4;

/// The genesis block's parent.
pub const NULL: Hash = [0u8; 32];

/// Core-serialized bytes of a toy block: `height` on `branch`, child of `parent`.
pub fn block_bytes(parent: &Hash, height: u32, branch: u8, size: usize) -> CoreBytes {
    assert!(size > FIXED + 3 && size - FIXED - 3 <= 0xffff, "toy block sizes are 1-64 KiB");
    let (width, script_len) = if size - FIXED - 1 <= 252 {
        (1, size - FIXED - 1)
    } else {
        (3, size - FIXED - 3)
    };
    let mut b = Vec::with_capacity(size);
    b.extend(1u32.to_le_bytes());
    b.extend_from_slice(parent);
    let mut root = [0u8; 32];
    root[..4].copy_from_slice(&height.to_le_bytes());
    root[4] = branch;
    b.extend_from_slice(&root);
    b.extend(height.to_le_bytes());
    b.extend(0u32.to_le_bytes());
    b.extend(u32::from(branch).to_le_bytes());
    b.push(1);
    b.extend(1u32.to_le_bytes());
    b.push(1);
    b.extend([0u8; 32]);
    b.extend(u32::MAX.to_le_bytes());
    b.extend([2, 0x51, 0x51]);
    b.extend(u32::MAX.to_le_bytes());
    b.push(1);
    b.extend(0i64.to_le_bytes());
    if width == 1 {
        b.push(script_len as u8);
    } else {
        b.push(0xfd);
        b.extend((script_len as u16).to_le_bytes());
    }
    b.push(0x6a);
    // Padding that differs from zero and between blocks, so a lost or stale sector anywhere in
    // a block reads back differently (an all-zero body would hide a torn record from R1).
    let seed = height.wrapping_mul(0x9e37_79b9) ^ u32::from(branch).wrapping_mul(0x85eb_ca6b);
    b.extend(
        (1..script_len as u32)
            .map(|i| (seed.wrapping_add(i.wrapping_mul(0x2545_f491)) >> 24) as u8 | 1),
    );
    b.extend(0u32.to_le_bytes());
    assert_eq!(b.len(), size);
    let mut input = b.as_slice();
    let bytes =
        dash_consensus::decode_block(&mut input).expect("a well-formed toy block").core_bytes();
    assert_eq!(bytes.as_slice(), b.as_slice());
    bytes
}

/// A block's id: `SHA256d` of its bytes.
pub fn block_id(bytes: &[u8]) -> Hash {
    sha256d::Hash::hash(bytes).to_byte_array()
}

/// The toy's undo record for a block: `len` bytes derived from its id (the toy's coins are
/// synthetic, so the undo is too; CS5a's codec replaces this in CH2).
pub fn undo_bytes(id: &Hash, len: usize) -> CoreUndoBytes {
    let v: Vec<u8> = (0..len).map(|i| id[i % 32] ^ (i as u8)).collect();
    CoreUndoBytes::from_raw(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_have_the_configured_size_and_distinct_ids() {
        let g = block_bytes(&NULL, 0, 0, 1024);
        assert_eq!(g.len(), 1024);
        let a = block_bytes(&block_id(g.as_slice()), 1, 0, 1024);
        let b = block_bytes(&block_id(g.as_slice()), 1, 1, 1024);
        assert_ne!(block_id(a.as_slice()), block_id(b.as_slice()));
        assert_eq!(block_bytes(&NULL, 0, 0, 900).len(), 900);
    }
}
