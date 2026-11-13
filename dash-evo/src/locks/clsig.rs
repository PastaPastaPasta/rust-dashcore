use std::fmt;

use dash_consensus::hash::sha256d;
use dash_p2p_core::ClSig;
use dash_pkc::bls::core::BlsSigValue;
use dash_pkc::bls::{BlsDecodeError, BlsEagerSig, BlsGlobalSig, BlsSchemeKind};
use dash_primitives::BlockHash;

use crate::bls::BlsCtx;

/// Core's decoded `chainlock::ChainLockSig` value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainLockSig {
    pub height: i32,
    pub block_hash: BlockHash,
    pub sig: BlsEagerSig,
}

impl ChainLockSig {
    /// Returns Core's null ChainLock signature.
    pub fn null() -> Self {
        Self {
            height: -1,
            block_hash: BlockHash::from([0; 32]),
            sig: BlsEagerSig::new(BlsSigValue::invalid(), BlsSchemeKind::Basic),
        }
    }

    /// Returns whether this is Core's null ChainLock signature.
    // Core: chainlock/chainlock.h:38 IsNull
    pub fn is_null(&self) -> bool {
        self.height == -1 && self.block_hash == BlockHash::from([0; 32])
    }

    /// Decodes the signature from the wire under the context captured at receipt.
    pub fn from_wire(message: &ClSig, read: BlsCtx) -> Result<Self, BlsDecodeError> {
        Ok(Self {
            height: message.height(),
            block_hash: message.block_hash(),
            sig: BlsGlobalSig::decode(message.sig().as_bytes())?.get(read),
        })
    }

    /// Re-encodes this decoded signature under the context used for sending.
    pub fn to_wire(&self, write: BlsCtx) -> ClSig {
        ClSig::new(self.height, self.block_hash, self.sig_bytes(write).into())
    }

    /// Returns the signature's wire bytes under the supplied context.
    pub fn sig_bytes(&self, write: BlsCtx) -> [u8; 96] {
        self.sig.to_bytes(write.scheme())
    }

    /// Serializes the decoded ChainLock signature as Core does for hashing.
    pub fn serialize(&self, write: BlsCtx) -> Vec<u8> {
        [
            self.height.to_le_bytes().as_slice(),
            self.block_hash.as_bytes(),
            self.sig_bytes(write).as_slice(),
        ]
        .concat()
    }

    /// Returns the double-SHA256 hash of the serialized ChainLock signature.
    pub fn serialize_hash(&self, write: BlsCtx) -> [u8; 32] {
        sha256d(&self.serialize(write))
    }
}

// Core: chainlock/chainlock.cpp:23-26 ChainLockSig::ToString
impl fmt::Display for ChainLockSig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ChainLockSig(nHeight={}, blockHash={})", self.height, self.block_hash)
    }
}

#[cfg(test)]
mod tests {
    use std::array;

    use dash_p2p_core::ClSig;
    use dash_pkc::bls::BlsSchemeKind;
    use dash_primitives::BlockHash;

    use super::*;
    use crate::locks::tests::sig;
    use crate::locks::{ClsigHashSchemes, clsig_hash};

    fn message(read: BlsCtx) -> ClSig {
        let value = BlsEagerSig::new(sig(1, &[0x5a; 32]), BlsSchemeKind::Basic);
        let hash: [u8; 32] = array::from_fn(|i| i as u8);
        ClSig::new(1234, BlockHash::from(hash), value.to_bytes(read.scheme()).into())
    }

    #[test]
    fn round_trips_basic_and_legacy_wire_values() {
        for read in [BlsCtx::BASIC, BlsCtx::LEGACY] {
            let wire = message(read);
            let value = ChainLockSig::from_wire(&wire, read).unwrap();
            assert!(value.sig.is_valid());
            assert_eq!(value.to_wire(read), wire);
        }
    }

    #[test]
    fn legacy_read_reencodes_to_basic_and_back() {
        let legacy = message(BlsCtx::LEGACY);
        let value = ChainLockSig::from_wire(&legacy, BlsCtx::LEGACY).unwrap();
        let basic = value.to_wire(BlsCtx::BASIC);
        assert_ne!(basic.sig().as_bytes(), legacy.sig().as_bytes());
        assert_eq!(basic, message(BlsCtx::BASIC));
        let reread = ChainLockSig::from_wire(&basic, BlsCtx::BASIC).unwrap();
        assert_eq!(reread, value);
        assert_eq!(reread.to_wire(BlsCtx::LEGACY), legacy);
        assert_eq!(&value.sig_bytes(BlsCtx::BASIC), basic.sig().as_bytes());
    }

    #[test]
    fn null_wire_is_zero_under_both_schemes() {
        let null = ChainLockSig::null();
        assert!(null.is_null());
        assert!(!null.sig.is_valid());
        assert_eq!(
            null.to_string(),
            format!("ChainLockSig(nHeight=-1, blockHash={})", BlockHash::from([0; 32]))
        );
        for write in [BlsCtx::BASIC, BlsCtx::LEGACY] {
            let wire = null.to_wire(write);
            assert_eq!(wire, ClSig::new(-1, BlockHash::from([0; 32]), [0; 96].into()));
            assert_eq!(ChainLockSig::from_wire(&wire, write).unwrap(), null);
        }
        let mut changed = null.clone();
        changed.height = 0;
        assert!(!changed.is_null());
        changed.height = -1;
        changed.block_hash = BlockHash::from([1; 32]);
        assert!(!changed.is_null());
        changed.block_hash = null.block_hash;
        changed.sig = ChainLockSig::from_wire(&message(BlsCtx::BASIC), BlsCtx::BASIC).unwrap().sig;
        assert!(changed.is_null());
    }

    #[test]
    fn serialize_hash_matches_clsig_hash() {
        for read in [BlsCtx::BASIC, BlsCtx::LEGACY] {
            for value in
                [ChainLockSig::null(), ChainLockSig::from_wire(&message(read), read).unwrap()]
            {
                for write in [BlsCtx::BASIC, BlsCtx::LEGACY] {
                    let bytes = value.serialize(write);
                    assert_eq!(bytes.len(), 132);
                    assert_eq!(&bytes[..4], &value.height.to_le_bytes());
                    assert_eq!(&bytes[4..36], value.block_hash.as_bytes());
                    assert_eq!(&bytes[36..], value.sig_bytes(write));
                    assert_eq!(
                        value.serialize_hash(write),
                        clsig_hash(&value.to_wire(write), ClsigHashSchemes::both(write)).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn equality_ignores_the_read_scheme() {
        let first = ChainLockSig::from_wire(&message(BlsCtx::LEGACY), BlsCtx::LEGACY).unwrap();
        let second = ChainLockSig::from_wire(&message(BlsCtx::BASIC), BlsCtx::BASIC).unwrap();
        assert_eq!(first.sig.scheme(), BlsSchemeKind::Legacy);
        assert_eq!(second.sig.scheme(), BlsSchemeKind::Basic);
        assert_eq!(first, second);
        let mut changed = second.clone();
        changed.height += 1;
        assert_ne!(first, changed);
        changed = second.clone();
        changed.block_hash = BlockHash::from([2; 32]);
        assert_ne!(first, changed);
        changed = second;
        changed.sig = BlsEagerSig::new(sig(2, &[0x5a; 32]), BlsSchemeKind::Basic);
        assert_ne!(first, changed);
    }

    #[test]
    fn wire_decode_normalizes_wrong_scheme_and_rejects_malformed() {
        let value = ChainLockSig::from_wire(&message(BlsCtx::BASIC), BlsCtx::LEGACY).unwrap();
        assert!(!value.sig.is_valid());
        for write in [BlsCtx::BASIC, BlsCtx::LEGACY] {
            assert_eq!(value.sig_bytes(write), [0; 96]);
        }
        let malformed = ClSig::new(1234, BlockHash::from([0xab; 32]), [0xff; 96].into());
        for read in [BlsCtx::BASIC, BlsCtx::LEGACY] {
            assert!(ChainLockSig::from_wire(&malformed, read).is_err());
        }
    }
}
