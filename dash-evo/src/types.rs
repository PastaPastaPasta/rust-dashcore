//! Core's vocabulary types that the masternode list and the SML share (EV addendum §1.2):
//! `proTxHash` and `nType` are the same Core fields in a `CDeterministicMN` and in a
//! `CSimplifiedMNListEntry` (`evo/dmn_types.h`). `dash_dml` re-exports them.

use std::fmt;

use dash_num::Hash256;

/// A masternode's `proTxHash`: the txid of its ProRegTx (Core `uint256`).
///
/// `Ord` is Core's `uint256::operator<`, a memcmp of the internal (little-endian) bytes
/// (`CompareByLastPaid`'s tie-break, the canonical snapshot sort); `Display` is Core's
/// byte-reversed hex. Neither is Core's iteration order, which is the DML's
/// `MnList::core_order` (quirk 41).
///
/// base-sdk has no `ProTxHash` yet; this newtype stands in for it (`API.md` Q5). It is not a
/// `make_hash!` type, whose expansion calls base-sdk's encoder, which pure crates may not.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProTxHash(Hash256);

impl ProTxHash {
    /// The internal (little-endian) bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Whether every byte is zero.
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// Core's `ImmerHasher`: `ReadLE64` of the first eight internal bytes, the 64-bit key of the
    /// CHAMP trie Core iterates in (EV2 §2.7; `evo/deterministicmns.h` v24:136-139).
    pub fn immer_hash(&self) -> u64 {
        let b = self.as_bytes();
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    }
}

impl From<[u8; 32]> for ProTxHash {
    /// From the internal (little-endian) bytes.
    fn from(bytes: [u8; 32]) -> Self {
        ProTxHash(Hash256::from(bytes))
    }
}

impl From<Hash256> for ProTxHash {
    fn from(hash: Hash256) -> Self {
        ProTxHash(hash)
    }
}

impl From<ProTxHash> for Hash256 {
    fn from(hash: ProTxHash) -> Self {
        hash.0
    }
}

impl fmt::Display for ProTxHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for ProTxHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProTxHash({})", self.0)
    }
}

/// One of the two masternode types Core defines (`MnType`, `evo/dmn_types.h` v24:30-43).
///
/// Core stores and compares `nType` as a raw `u16`: its reader accepts any value
/// (`deterministicmns.h` v24:69-84), so the DML's `Dmn` holds a [`RawMnType`] and this enum names
/// the two defined ones ([`RawMnType::stored_type`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MnType {
    /// `MnType::Regular` (0): weight 1, collateral 1000 DASH.
    Regular,
    /// `MnType::Evo` (1): weight 4, collateral 4000 DASH.
    Evo,
}

impl MnType {
    /// The `u16` Core serializes (`nType`).
    pub const fn to_raw(self) -> RawMnType {
        match self {
            MnType::Regular => RawMnType(0),
            MnType::Evo => RawMnType(1),
        }
    }

    /// The defined type for a raw value; `None` for any other value.
    pub const fn from_raw(raw: RawMnType) -> Option<MnType> {
        match raw.0 {
            0 => Some(MnType::Regular),
            1 => Some(MnType::Evo),
            _ => None,
        }
    }

    /// The voting weight (`GetMnType(nType).voting_weight`).
    pub const fn weight(self) -> u32 {
        match self {
            MnType::Regular => 1,
            MnType::Evo => 4,
        }
    }
}

/// `nType` as Core holds it: any `u16`, in a payload (EVO-15 compares out-of-range values) and in
/// a stored masternode (Core's reader accepts any value; Rule DDE keeps that domain).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawMnType(pub u16);

impl RawMnType {
    /// `MnType::Regular`.
    pub const REGULAR: RawMnType = MnType::Regular.to_raw();
    /// `MnType::Evo`.
    pub const EVO: RawMnType = MnType::Evo.to_raw();

    /// The defined type, if the value is one (`IsValidMnType`).
    pub const fn stored_type(self) -> Option<MnType> {
        MnType::from_raw(self)
    }

    /// `nType == MnType::Evo`.
    pub const fn is_evo(self) -> bool {
        self.0 == Self::EVO.0
    }

    /// `GetMnType(nType).voting_weight`: 1 or 4, and 0 for any undefined value
    /// (`dmn_types::Invalid`, `evo/dmn_types.h` v24:41-62).
    pub const fn weight(self) -> u32 {
        match self.stored_type() {
            Some(t) => t.weight(),
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pro_tx_hash_orders_by_internal_bytes() {
        // Display is byte-reversed; Ord is the memcmp of the internal bytes (EV2 §2.2).
        let mut low = [0u8; 32];
        low[31] = 1;
        let mut high = [0u8; 32];
        high[0] = 2;
        let (low, high) = (ProTxHash::from(low), ProTxHash::from(high));
        assert!(low < high);
        assert!(low.to_string().starts_with("01"), "{low}");
        assert_eq!(high.immer_hash(), 2);
        assert_eq!(low.immer_hash(), 0);
    }

    #[test]
    fn stored_types_are_the_two_core_values() {
        assert_eq!(MnType::from_raw(RawMnType(0)), Some(MnType::Regular));
        assert_eq!(MnType::from_raw(RawMnType(1)), Some(MnType::Evo));
        assert_eq!(MnType::from_raw(RawMnType(7)), None);
        assert_eq!(MnType::Evo.to_raw(), RawMnType(1));
    }
}
