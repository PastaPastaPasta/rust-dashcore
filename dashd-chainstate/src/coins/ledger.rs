//! [`LedgerBox`]: a long undo coin's bytes, counted in the native ledger while they live (CH2 §2.5
//! "Ownership"; rev4, G-R4-m2).
//!
//! A block overlay's `spend` returns the coin it spent for the block's undo. A long one owns its
//! bytes in a `LedgerBox`, which adds them to the cache's [`HeldOut`] when it is created and takes
//! them off when it is dropped, on whatever thread and in whichever component drops it (CH3a's undo
//! record, or the overlay's error path). So the tagged coins bytes equal native Σ − R at every op
//! boundary, undo included.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::alloc_tag;

/// The bytes of the live [`LedgerBox`]es one cache handed out: `Usage::held_out`.
#[derive(Clone, Debug, Default)]
pub struct HeldOut(Arc<AtomicU64>);

impl HeldOut {
    pub fn bytes(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A long coin's bytes, counted in a [`HeldOut`] until dropped.
pub struct LedgerBox {
    bytes: Box<[u8]>,
    ledger: HeldOut,
}

impl LedgerBox {
    /// Counts `bytes`, which were allocated under the coins tag (copied there, or moved out of a
    /// layer), in `ledger`.
    pub(crate) fn new(bytes: Box<[u8]>, ledger: &HeldOut) -> Self {
        ledger.0.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        Self {
            bytes,
            ledger: ledger.clone(),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Clone for LedgerBox {
    fn clone(&self) -> Self {
        let _tag = alloc_tag::coins();
        Self::new(self.bytes.clone(), &self.ledger)
    }
}

impl Drop for LedgerBox {
    fn drop(&mut self) {
        self.ledger.0.fetch_sub(self.bytes.len() as u64, Ordering::Relaxed);
    }
}

#[cfg(all(test, feature = "alloc-tag"))]
mod tests {
    use super::*;
    use crate::alloc_tag::{Tag, new_domain, set_thread_domain, tagged_bytes};

    #[test]
    fn a_box_is_counted_until_dropped_on_any_thread() {
        let domain = new_domain();
        set_thread_domain(domain);
        let held = HeldOut::default();
        let b = {
            let _tag = alloc_tag::coins();
            LedgerBox::new(vec![7; 100].into_boxed_slice(), &held)
        };
        let c = b.clone();
        assert_eq!(held.bytes(), 200);
        assert_eq!(tagged_bytes(domain, Tag::Coins), 200);
        #[expect(
            clippy::disallowed_methods,
            reason = "a test thread; LO-9 governs node threads (CH1-node §8.3)"
        )]
        std::thread::spawn(move || drop((b, c))).join().unwrap();
        assert_eq!(held.bytes(), 0);
        assert_eq!(tagged_bytes(domain, Tag::Coins), 0);
    }
}
