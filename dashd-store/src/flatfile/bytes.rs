//! The only byte types the block-file writer accepts (DEC-30 N3): stored block and undo bytes must
//! be Core's serialization, so the writer takes these types rather than `&[u8]`.
//!
//! - [`CoreBytes`] is CS0's `dash_consensus::CoreBytes` (re-exported): made only by
//!   `core_bytes(&value)` or `Decoded::core_bytes()`, never from raw bytes.
//! - [`CoreUndoBytes`] is a placeholder with the same shape until CS5a's `CBlockUndo` codec lands
//!   (TODO(CS5a): replace it with that codec's type, and drop the `raw-bytes` feature, which now
//!   only provides `CoreUndoBytes::from_raw` for the byte-identity tests and the bench).
//!
//! CH5's `-reindex` must pass the scanner's exact block bytes (or `decode_block` of the record) to
//! `save_block(known_pos)`, never the whole record: a size field larger than the block would
//! inflate the file's size statistics.

use std::sync::Arc;

pub use dash_consensus::CoreBytes;

/// Bytes equal to Core's serialization of a `CBlockUndo` (what a rev record holds before its
/// checksum).
#[derive(Clone, PartialEq, Eq)]
pub struct CoreUndoBytes(Arc<[u8]>);

macro_rules! byte_type {
    ($t:ident) => {
        impl $t {
            pub fn as_slice(&self) -> &[u8] {
                &self.0
            }

            pub fn len(&self) -> usize {
                self.0.len()
            }

            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            /// Wraps bytes the caller asserts are Core's serialization. Tests only.
            #[cfg(any(test, feature = "raw-bytes"))]
            pub fn from_raw(bytes: impl Into<Arc<[u8]>>) -> Self {
                Self(bytes.into())
            }
        }

        impl AsRef<[u8]> for $t {
            fn as_ref(&self) -> &[u8] {
                &self.0
            }
        }

        impl std::fmt::Debug for $t {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!(stringify!($t), "({} bytes)"), self.0.len())
            }
        }
    };
}

byte_type!(CoreUndoBytes);
