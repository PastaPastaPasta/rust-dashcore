//! Core's canonical bytes and the consensus objects built on them (CS0 §4–§6).
//!
//! Core hashes its own re-serialization of a decoded object, stores it and relays it; the node
//! does the same (A1 §1.3). Txids, instance hashes, merkle leaves, the sighash preimage, block
//! storage and relay all take their bytes from [`core_serialize`], and a final commitment's hash
//! from [`commitment_hash`], never from base-sdk's encoders.

use core::cmp::Ordering;

/// Compare Core's internal `uint256` bytes, as `base_blob::Compare` does.
// Core: v24.0.0 src/uint256.h:55-59
pub fn core_hash_cmp(a: &[u8; 32], b: &[u8; 32]) -> Ordering {
    a.as_slice().cmp(b.as_slice())
}

mod block;
pub mod canonical;
mod commitment;
mod core_payload;
pub(crate) mod decode;
mod protx;
pub(crate) mod read;
pub(crate) mod serialize;
mod tx;

pub use block::{Block, Header};
pub use commitment::{commitment_hash, write_commitment};
pub use core_payload::{
    CommitmentBls, CorePayload, MnListDiffBls, P2pCommitment, P2pMnListDiff, P2pQrInfo,
    pro_tx_operator_key, pro_tx_operator_sig, protx_key_scheme, shared_operator_key,
};
pub use decode::{
    DecodeError, DecodeErrorKind, Decoded, MAX_SIZE, decode_block, decode_header, decode_tx,
    decode_tx_exact,
};
pub use protx::{
    ProRegTxUnsigned, ProUpRegTxUnsigned, ProUpSharedRegTxUnsigned, SharedRegConsent,
    write_net_info,
};
pub use read::{ReadError, ReadErrorKind, read_compact_size, read_varint_u32, read_varint_u64};
pub use serialize::{
    ArrayWriter, CompareWriter, CoreBytes, CoreSerialize, CoreWrite, HashWriter, SizeWriter,
    compact_size_len, core_bytes, core_serialize, header_bytes, varint_len, write_bytes,
    write_compact_size, write_dyn_bitset, write_varint,
};
pub use tx::{Tx, TxRef};

/// base-sdk's decode error, which [`DecodeErrorKind::Codec`] carries.
pub use dash_types::codec::DecodeError as CodecError;

pub use dash_primitives::{
    BlockHash, BlockHeader, MerkleRoot, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType,
};

#[cfg(test)]
mod tests {
    use super::core_hash_cmp;
    use core::cmp::Ordering;

    #[test]
    fn core_hash_cmp_is_raw_byte_order() {
        let mut first = [0; 32];
        first[0] = 1;
        let mut last = [0; 32];
        last[31] = 1;
        assert_eq!(core_hash_cmp(&first, &last), Ordering::Greater);
        assert_eq!(core_hash_cmp(&last, &first), Ordering::Less);
    }
}
