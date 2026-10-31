//! The node's EV1 checks, and under `test-support` L3's E-evo gate counters around them (CH3a a10
//! track L plan §3.6, OD-L3-17/18): `Counting` wraps [`DmlChecks`], counts each key's
//! predicate from the call's own arguments, read-only, then delegates unchanged.
//!
//! A product build's [`NodeEv1Checks`] is [`DmlChecks`] itself; a `test-support` build's is
//! `Counting`, whose counts are registered under the chainstate's data directory for the
//! harness to read (`counts_under`). The node constructs either through [`node_ev1_checks`], so
//! it carries no `cfg` of its own.
//!
//! The keys (`w.` a wrapper entered, `g.` a DIP3-gated body entered, `d.` a diagnostic):
//! - `d.stx_hook`: every step-4 call;
//! - `g.coll`: the V24 shared-collateral rules, the step-4 template check under `rules.v24`
//!   (st:918) and every H8 call (v:2719);
//! - `w.stx`: CheckSpecialTxInner's entry: a step-4 call past the template check and past the
//!   skip of a raw-type-5 `vtx[0]` (ev1.rs:254-255, st:912-930);
//! - `g.stx`: a `w.stx` entry with DIP3 active and a special transaction (ev1.rs:258-263,
//!   st:224-232);
//! - `w.cbtx`: every step-1 call (st:876-892);
//! - `g.cbtx`: a step-1 call whose CbTx decodes (st:876-892; V gates steps 5 and 8-10 on it).
//!
//! [`DmlChecks`]: super::ev1::DmlChecks

use std::path::Path;

/// The EV1 checks the node's evo hooks run.
#[cfg(not(feature = "test-support"))]
pub type NodeEv1Checks = super::ev1::DmlChecks;

/// The EV1 checks the node's evo hooks run: counted, under `test-support`.
#[cfg(feature = "test-support")]
pub type NodeEv1Checks = Counting;

/// The node's EV1 checks for a chainstate at `data_dir`.
#[cfg(not(feature = "test-support"))]
pub fn node_ev1_checks(_data_dir: &Path) -> NodeEv1Checks {
    super::ev1::DmlChecks
}

/// The node's EV1 checks for a chainstate at `data_dir`, their counts registered under it.
#[cfg(feature = "test-support")]
pub fn node_ev1_checks(data_dir: &Path) -> NodeEv1Checks {
    Counting::registered(data_dir)
}

#[cfg(feature = "test-support")]
pub use counting::{Counting, GateCounts, KEYS, counts_under};

#[cfg(feature = "test-support")]
mod counting {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, Weak};

    use dash_consensus::coins::CoinsView;
    use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
    use dash_consensus::{Block, Tx, TxValidationError};
    use dash_dml::specialtx::{BlockPayloads, cbtx_payload, shared_collateral_template_outputs};
    use dash_primitives::CoinbaseCommitment;
    use dash_types::Numeric as _;

    use super::super::ev1::{DmlChecks, Ev1Checks, SpecialTxCtx};
    use crate::chain::Fatal;
    use crate::connect::SpecialTxFailure;
    use crate::store::CoinsReadError;

    /// The counted keys, in [`GateCounts::snapshot`]'s order.
    pub const KEYS: [&str; 6] = ["d.stx_hook", "g.coll", "w.stx", "g.stx", "w.cbtx", "g.cbtx"];

    /// One count per key.
    #[derive(Debug, Default)]
    pub struct GateCounts([AtomicU64; 6]);

    impl GateCounts {
        fn bump(&self, key: &str) {
            let i = KEYS.iter().position(|k| *k == key).expect("a counted key");
            self.0[i].fetch_add(1, Ordering::Relaxed);
        }

        /// Every key's count.
        pub fn snapshot(&self) -> BTreeMap<&'static str, u64> {
            KEYS.iter().zip(&self.0).map(|(k, n)| (*k, n.load(Ordering::Relaxed))).collect()
        }
    }

    /// Each live [`Counting`]'s counts by its chainstate's data directory, newest last.
    static REGISTRY: Mutex<Vec<(PathBuf, Weak<GateCounts>)>> = Mutex::new(Vec::new());

    /// The newest live counts of a chainstate whose data directory is `dir` or below it.
    pub fn counts_under(dir: &Path) -> Option<Arc<GateCounts>> {
        let registry = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
        registry.iter().rev().filter(|(d, _)| d.starts_with(dir)).find_map(|(_, c)| c.upgrade())
    }

    /// [`DmlChecks`], counted.
    pub struct Counting {
        inner: DmlChecks,
        counts: Arc<GateCounts>,
    }

    impl Counting {
        /// [`DmlChecks`] with new counts of its own, registered nowhere.
        pub fn new() -> Counting {
            Counting {
                inner: DmlChecks,
                counts: Arc::default(),
            }
        }

        /// New counts, registered under `data_dir` (dead entries pruned).
        pub fn registered(data_dir: &Path) -> Counting {
            let counting = Counting::new();
            let mut registry = REGISTRY.lock().unwrap_or_else(|p| p.into_inner());
            registry.retain(|(_, c)| c.strong_count() > 0);
            registry.push((data_dir.to_owned(), Arc::downgrade(&counting.counts)));
            counting
        }

        pub fn counts(&self) -> &Arc<GateCounts> {
            &self.counts
        }
    }

    impl Default for Counting {
        fn default() -> Counting {
            Counting::new()
        }
    }

    impl Ev1Checks for Counting {
        type Payloads<'b> = BlockPayloads<'b>;

        fn payloads<'b>(&self, block: &'b Block) -> BlockPayloads<'b> {
            self.inner.payloads(block)
        }

        fn check_cbtx(
            &self,
            block: &Block,
            payloads: &BlockPayloads<'_>,
            cx: SpecialTxCtx<'_>,
        ) -> Result<Result<CoinbaseCommitment, SpecialTxFailure>, Fatal> {
            self.counts.bump("w.cbtx");
            if let Some(tx) = block.txs().first()
                && cbtx_payload(tx, payloads.get(0)).is_ok()
            {
                self.counts.bump("g.cbtx");
            }
            self.inner.check_cbtx(block, payloads, cx)
        }

        /// The adapter's order (ev1.rs:244-263): the template check under `rules.v24`, then the
        /// raw-type-5 `vtx[0]` skip, then CheckSpecialTxInner's entry.
        fn check_tx(
            &self,
            block: &Block,
            payloads: &BlockPayloads<'_>,
            i: usize,
            cx: SpecialTxCtx<'_>,
        ) -> Result<Result<(), SpecialTxFailure>, Fatal> {
            self.counts.bump("d.stx_hook");
            if let Some(tx) = block.txs().get(i) {
                let template_ok = if cx.rules.v24 {
                    self.counts.bump("g.coll");
                    shared_collateral_template_outputs(tx, payloads.get(i)).is_ok()
                } else {
                    true
                };
                let skipped = i == 0 && tx.tx_type().to_base() == TRANSACTION_COINBASE;
                if template_ok && !skipped {
                    self.counts.bump("w.stx");
                    if cx.dip3_active() && tx.has_extra_payload_field() {
                        self.counts.bump("g.stx");
                    }
                }
            }
            self.inner.check_tx(block, payloads, i, cx)
        }

        fn shared_collateral_spends(
            &self,
            tx: &Tx,
            view: &dyn CoinsView<Error = CoinsReadError>,
        ) -> Result<Result<(), TxValidationError>, Fatal> {
            self.counts.bump("g.coll");
            self.inner.shared_collateral_spends(tx, view)
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "gates_tests.rs"]
mod tests;
