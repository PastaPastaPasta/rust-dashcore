//! Core's canonical bytes and the consensus objects built on them (CS0 §4–§6).
//!
//! Core hashes its own re-serialization of a decoded object, stores it and relays it; the node
//! does the same (A1 §1.3). Txids, instance hashes, merkle leaves, the sighash preimage, block
//! storage and relay all take their bytes from [`core_serialize`], and a final commitment's hash
//! from [`commitment_hash`], never from base-sdk's encoders.

mod block;
pub mod canonical;
mod commitment;
pub(crate) mod decode;
pub(crate) mod read;
pub(crate) mod serialize;
mod tx;

pub use block::{Block, Header};
pub use commitment::{commitment_hash, write_commitment};
pub use decode::{
    DecodeError, DecodeErrorKind, Decoded, MAX_SIZE, decode_block, decode_header, decode_tx,
    decode_tx_exact,
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
