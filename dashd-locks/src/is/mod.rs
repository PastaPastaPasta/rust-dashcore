//! InstantSend's shared state and seams (LK1b-s).

pub mod batch;
pub mod chain;
pub mod db;
pub mod fx;
pub mod listener;
pub mod manager;
pub mod net;
pub mod seams;
pub mod verify;
pub mod worker;

#[cfg(test)]
mod s3_fixtures;

use dash_p2p_core::IsdLock;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// The bytes returned by [`dash_evo::locks::isdlock_hash`] for Core's serialized lock.
pub type IsLockHash = [u8; 32];

/// The lock object associated with a transaction.
pub type LockedTx = IsdLock;

/// The InstantSend tip height cache (Core's `GetTipHeight`, defaulting to zero before a tip).
#[derive(Clone, Debug, Default)]
pub struct IsTipCache(Arc<AtomicI32>);

impl IsTipCache {
    /// Construct an empty cache whose height is zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the cached tip height.
    pub fn get(&self) -> i32 {
        self.0.load(Ordering::Acquire).max(0)
    }

    /// Publish a new cached tip height.
    pub fn set(&self, height: i32) {
        self.0.store(height, Ordering::Release);
    }
}

pub use dash_consensus::connect::lock_inputs;

/// The process flags used by Core's `IsInstantSendEnabled` gate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IsGate {
    /// Whether SPORK_2 (InstantSend) is active.
    pub spork2: bool,
    /// Whether block reindexing is in progress.
    pub reindex: bool,
    /// Whether blocks are being imported.
    pub importing: bool,
}

impl IsGate {
    /// Construct a gate from Core's three inputs.
    pub const fn new(spork2: bool, reindex: bool, importing: bool) -> Self {
        Self {
            spork2,
            reindex,
            importing,
        }
    }

    /// Core's `IsInstantSendEnabled`: SPORK_2 and neither loading flag.
    // Core: instantsend/instantsend.cpp:513-516
    pub const fn enabled(self) -> bool {
        self.spork2 && !self.reindex && !self.importing
    }
}

#[cfg(test)]
mod tests {
    use super::{IsGate, IsTipCache, lock_inputs};
    use dash_consensus::primitives::{Transaction, TxHash, TxType};

    #[test]
    fn is_gate_truth_table() {
        for (spork2, reindex, importing, enabled) in [
            (false, false, false, false),
            (false, false, true, false),
            (false, true, false, false),
            (false, true, true, false),
            (true, false, false, true),
            (true, false, true, false),
            (true, true, false, false),
            (true, true, true, false),
        ] {
            assert_eq!(IsGate::new(spork2, reindex, importing).enabled(), enabled);
        }
    }

    #[test]
    fn tip_cache_clamps_core_no_tip_value() {
        let cache = IsTipCache::new();
        cache.set(-1);
        assert_eq!(cache.get(), 0);
    }

    #[test]
    fn lock_inputs_uses_asset_unlock_request_outpoint() {
        let mut payload = vec![0; dash_consensus::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE];
        payload[1..9].copy_from_slice(&42u64.to_le_bytes());
        let tx = dash_consensus::Tx::new(Transaction::new(
            3,
            TxType::new(9),
            vec![],
            vec![],
            0,
            payload,
        ));
        assert_eq!(lock_inputs(&tx).len(), 1);
        assert_eq!(lock_inputs(&tx)[0].index(), 0);
        // SHA256d(0x06 || "plwdtx" || 42u64 LE), computed outside this crate.
        let expected = [
            0xc6, 0x94, 0x69, 0x5f, 0x42, 0xd2, 0x67, 0x67, 0x58, 0x3b, 0x5c, 0x7a, 0x55, 0xdd,
            0x05, 0xbb, 0x60, 0x68, 0x1d, 0xd0, 0x8a, 0x45, 0x8e, 0x70, 0x7f, 0x0a, 0x88, 0x85,
            0x0f, 0x08, 0xcb, 0x54,
        ];
        assert_eq!(lock_inputs(&tx)[0].txid(), TxHash::from(expected));
    }

    #[test]
    fn lock_inputs_falls_back_to_transaction_inputs_for_wrong_payload_size() {
        let previous = dash_consensus::primitives::OutPoint::new(TxHash::from([9; 32]), 3);
        let tx = dash_consensus::Tx::new(Transaction::new(
            3,
            TxType::new(9),
            vec![dash_consensus::primitives::TxIn::new(previous, Default::default(), u32::MAX)],
            vec![],
            0,
            vec![0; 8],
        ));
        assert_eq!(lock_inputs(&tx), vec![previous]);
    }
}
