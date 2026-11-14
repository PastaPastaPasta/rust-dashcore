//! Mutable signature-share inventories, with Core's resize and merge semantics.

use super::state::UNINITIALIZED_SESSION_ID;
use crate::dkg::wire::{SigSharesBits, SigSharesInv as WireSigSharesInv};

/// Core's in-memory `CSigSharesInv`; the wire inventory is built when sending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigSharesInv {
    pub session_id: u32,
    pub inv: Vec<bool>,
}

impl Default for SigSharesInv {
    // Core: v24.0.0 l/signing_shares.h:131-132 CSigSharesInv
    fn default() -> Self {
        Self {
            session_id: UNINITIALIZED_SESSION_ID,
            inv: Vec::new(),
        }
    }
}

impl SigSharesInv {
    /// Copy the incoming inventory into mutable session state.
    // Core: v24.0.0 l/signing_shares.h:135-145 CSigSharesInv::SerializationOp
    pub fn from_wire(inv: &WireSigSharesInv) -> Self {
        let bits = inv.inv();
        Self {
            session_id: inv.session_id(),
            inv: (0..bits.num_bits()).map(|i| bits.get(i).unwrap_or(false)).collect(),
        }
    }

    /// Build the wire inventory, with unused padding bits clear.
    // Core: v24.0.0 l/signing_shares.h:135-145 CSigSharesInv::SerializationOp
    pub fn to_wire(&self) -> WireSigSharesInv {
        let mut data = vec![0; self.inv.len().div_ceil(8)];
        for (i, set) in self.inv.iter().enumerate() {
            if *set {
                data[i / 8] |= 1 << (i % 8);
            }
        }
        let bits = SigSharesBits::new(self.inv.len() as u64, data)
            .expect("inventory has clear padding bits");
        WireSigSharesInv::new(self.session_id, bits)
    }

    // Core: v24.0.0 l/signing_shares.cpp:96-99 CSigSharesInv::Init
    pub fn init(&mut self, size: usize) {
        self.inv.resize(size, false);
    }

    // Core: v24.0.0 l/signing_shares.cpp:101-105 CSigSharesInv::Set
    pub fn set(&mut self, member: u16, value: bool) {
        self.inv[usize::from(member)] = value;
    }

    // Core: v24.0.0 l/signing_shares.cpp:107-110 CSigSharesInv::SetAll
    pub fn set_all(&mut self, value: bool) {
        self.inv.fill(value);
    }

    // Core: v24.0.0 l/signing_shares.cpp:63-70 CSigSharesInv::Merge
    pub fn merge(&mut self, other: &Self) {
        assert!(other.inv.len() >= self.inv.len());
        for (bit, other_bit) in self.inv.iter_mut().zip(&other.inv) {
            *bit |= other_bit;
        }
    }

    // Core: v24.0.0 l/signing_shares.cpp:72-75 CSigSharesInv::CountSet
    pub fn count_set(&self) -> usize {
        self.inv.iter().filter(|bit| **bit).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inv_matches_core_set_merge_count_and_resize() {
        let mut inv = SigSharesInv::default();
        assert_eq!(inv.session_id, u32::MAX);
        assert_eq!(inv.count_set(), 0);
        inv.session_id = 42;
        inv.init(6);
        inv.set(0, true);
        inv.set(3, true);
        inv.set(5, true);
        inv.set(3, false);
        assert_eq!(inv.count_set(), 2);
        let mut other = SigSharesInv {
            session_id: 77,
            inv: vec![false; 7],
        };
        other.set(2, true);
        other.set(5, true);
        other.set(6, true);
        inv.merge(&other);
        assert_eq!(inv.inv, [true, false, true, false, false, true]);
        assert_eq!(inv.session_id, 42);
        assert_eq!(inv.count_set(), 3);
        inv.merge(&other);
        assert_eq!(inv.count_set(), 3);
        inv.init(3);
        assert_eq!(inv.inv, [true, false, true]);
        inv.init(5);
        assert_eq!(inv.inv, [true, false, true, false, false]);
        inv.set_all(true);
        assert_eq!(inv.count_set(), 5);
        inv.set_all(false);
        assert_eq!(inv.count_set(), 0);
        inv.init(0);
        inv.merge(&SigSharesInv::default());
        assert_eq!(inv.count_set(), 0);
    }

    #[test]
    fn inventory_wire_round_trip_preserves_bits_and_session_id() {
        for size in 0..=12 {
            for pattern in [0u16, 1, 0x555, 0xaaa, 0xfff] {
                let inv = SigSharesInv {
                    session_id: 73,
                    inv: (0..size).map(|i| pattern & (1 << i) != 0).collect(),
                };
                let wire = inv.to_wire();
                assert_eq!(wire.session_id(), 73);
                assert_eq!(wire.inv().num_bits(), size as u64);
                assert_eq!(SigSharesInv::from_wire(&wire), inv);
                assert_eq!(inv.count_set(), inv.inv.iter().filter(|set| **set).count());
            }
        }
    }

    #[test]
    #[should_panic]
    fn inv_set_rejects_member_at_size() {
        let mut inv = SigSharesInv::default();
        inv.init(3);
        inv.set(3, true);
    }
}
