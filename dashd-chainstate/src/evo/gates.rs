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
//!   (st:916-921) and every H8 call (v:2719), both at v24.0.0;
//! - `w.stx`: CheckSpecialTxInner's entry: a step-4 call past the template check and past the
//!   skip of a raw-type-5 `vtx[0]` (ev1.rs:254-255, st:913-931 at v24.0.0);
//! - `g.stx`: a `w.stx` entry with DIP3 active and a special transaction (ev1.rs:258-263,
//!   st:224-232 at v24.0.0);
//! - `w.cbtx`: every step-1 call (st:876-892 at v24.0.0);
//! - `g.cbtx`: a step-1 call whose CbTx decodes (st:876-892 at v24.0.0; V gates steps 5 and 8-10 on
//!   it).
//!
//! BL-564's five, which `NodeEvo`'s H4 counts in the same cells (CH7b-2 C8, C8b) when Core's call
//! is made, not when it succeeds (Core v24.0.0-rc.4):
//! - `w.quorum`: `CQuorumBlockProcessor::ProcessBlock` called (`evo/specialtxman.cpp:955`), on
//!   every block that reaches step 6;
//! - `g.quorum`: past its DIP0003 return (`llmq/blockprocessor.cpp:218-221`), a proposal past
//!   DIP0003 included;
//! - `g.dmn`: step 7's DIP0003 block entered (`evo/specialtxman.cpp:966`), before
//!   `BuildNewListFromBlock` (`:967`), whatever it returns, and under `fJustCheck` too;
//! - `w.mnhf`: `CMNHFManager::ProcessBlock` called (`evo/specialtxman.cpp:1033`), on every block
//!   that reaches step 11, a proposal included;
//! - `g.mnhf`: past `extractSignals` (`evo/mnhftx.cpp:214`) and its no-signal return
//!   (`:219-225`), so before `bad-mnhf-duplicate`, `bad-mnhf-non-mn-fork` and the `fJustCheck`
//!   return.
//!
//! H9 counts none of them.
//!
//! A proposal (`TestBlockValidity`, [`AttemptOrigin::Proposal`]) counts none of the eleven: each
//! of its entries counts the key's `p.` mirror (`p.w.quorum` for `w.quorum`, and so on), at the
//! same point and with the same meaning (OD-L3-43). `NodeEvo` puts the cells in proposal mode
//! for the attempt (`begin_attempt` to `finish_attempt`), so the seam's counts follow too.
//!
//! The cells live as long as their checks: a restart registers new ones, all twenty-two keys
//! together.
//!
//! [`AttemptOrigin::Proposal`]: crate::chain::AttemptOrigin::Proposal
//!
//! Beside them, T-TN's root record (CH7b-2 C16, `RootChecks`): the heights at which H4 compared
//! a CbTx's roots, step 8's `merkleRootMNList` (`evo/specialtxman.cpp:984-996` at v24.0.0) and step
//! 9's `merkleRootQuorums` (`:1003-1011`), and those it judged without `fCheckCbTxMerkleRoots`; and
//! the heights at which step 10 returned at one of its best-ChainLock shortcuts (`:152-158`,
//! `:187-193`), with which (CH7b-3 C2); and in unit tests the step trace, each pass of step 10
//! (`:1020`) and step 11 (`:1033-1035` at v24.0.0) by height (CH7b-3 C3).
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

#[cfg(any(test, feature = "test-support"))]
pub use cells::{EvoEdgeCounts, GateCounts, KEYS, RootChecks};
#[cfg(feature = "test-support")]
pub use counting::{Counting, counts_under};

/// The cells, which a plain `cfg(test)` build's `NodeEvo` also counts in (it has no `Counting`).
#[cfg(any(test, feature = "test-support"))]
mod cells {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// The connect keys: the EV1 seam's six, then `NodeEvo`'s five.
    const CONNECT: usize = 11;

    /// The counted keys, in [`GateCounts::snapshot`]'s order: the EV1 seam's six, then
    /// `NodeEvo`'s five, then a proposal's `p.` mirror of each, in the same order (OD-L3-43).
    pub const KEYS: [&str; 2 * CONNECT] = [
        "d.stx_hook",
        "g.coll",
        "w.stx",
        "g.stx",
        "w.cbtx",
        "g.cbtx",
        "w.quorum",
        "g.quorum",
        "g.dmn",
        "w.mnhf",
        "g.mnhf",
        "p.d.stx_hook",
        "p.g.coll",
        "p.w.stx",
        "p.g.stx",
        "p.w.cbtx",
        "p.g.cbtx",
        "p.w.quorum",
        "p.g.quorum",
        "p.g.dmn",
        "p.w.mnhf",
        "p.g.mnhf",
    ];

    /// One monotonic count per key, the root record, and whether a proposal's attempt is open.
    #[derive(Debug, Default)]
    pub struct GateCounts([AtomicU64; 2 * CONNECT], Mutex<RootChecks>, AtomicBool);

    /// The heights, in call order, at which H4 ran step 8 (every CbTx it checked), ran step 9
    /// (a CbTx of version 2 or more), and judged a block without `fCheckCbTxMerkleRoots`, where
    /// it runs neither; where step 10 took one of its best-ChainLock shortcuts, and which; where
    /// it asked for the best ChainLock, with the answer's height (`None` for Core's null lock;
    /// a failed ask is not kept); and each pass of step 10 (a CbTx it judged) and of step 11, as
    /// `(height, step)`.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct RootChecks {
        pub mn_list: Vec<u32>,
        pub quorums: Vec<u32>,
        pub unchecked: Vec<u32>,
        pub cl_shortcuts: Vec<(u32, crate::evo::cl::ClPass)>,
        pub cl_asks: Vec<(u32, Option<i32>)>,
        /// Unit tests only: a long run would hold two entries a block.
        #[cfg(test)]
        pub steps: Vec<(u32, u8)>,
    }

    impl GateCounts {
        /// Counts the connect key `key`, or its `p.` mirror while a proposal's attempt is open.
        pub(crate) fn bump(&self, key: &str) {
            let mirror = if self.2.load(Ordering::Relaxed) {
                CONNECT
            } else {
                0
            };
            self.0[index(key) + mirror].fetch_add(1, Ordering::Relaxed);
        }

        /// Whether the attempt now open is a proposal's: V runs one attempt at a time.
        pub(crate) fn set_proposal(&self, proposal: bool) {
            self.2.store(proposal, Ordering::Relaxed);
        }

        pub(crate) fn record_roots(&self, f: impl FnOnce(&mut RootChecks)) {
            f(&mut self.1.lock().unwrap_or_else(|p| p.into_inner()));
        }

        /// The root record so far.
        pub fn root_checks(&self) -> RootChecks {
            self.1.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }

        fn get(&self, key: &str) -> u64 {
            let i = KEYS.iter().position(|k| *k == key).expect("a counted key");
            self.0[i].load(Ordering::Relaxed)
        }

        /// Every key's count.
        pub fn snapshot(&self) -> BTreeMap<&'static str, u64> {
            KEYS.iter().zip(&self.0).map(|(k, n)| (*k, n.load(Ordering::Relaxed))).collect()
        }

        /// `NodeEvo`'s five counts.
        pub fn edges(&self) -> EvoEdgeCounts {
            self.edges_with("")
        }

        /// `NodeEvo`'s five counts' `p.` mirrors: the edges proposals entered.
        pub fn proposal_edges(&self) -> EvoEdgeCounts {
            self.edges_with("p.")
        }

        fn edges_with(&self, prefix: &str) -> EvoEdgeCounts {
            let get = |key: &str| self.get(&format!("{prefix}{key}"));
            EvoEdgeCounts {
                w_quorum: get("w.quorum"),
                g_quorum: get("g.quorum"),
                g_dmn: get("g.dmn"),
                w_mnhf: get("w.mnhf"),
                g_mnhf: get("g.mnhf"),
            }
        }
    }

    /// A connect key's index; its mirror's is [`CONNECT`] further.
    fn index(key: &str) -> usize {
        KEYS[..CONNECT].iter().position(|k| *k == key).expect("a connect key")
    }

    /// [`GateCounts`]' five `NodeEvo` keys.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct EvoEdgeCounts {
        /// `w.quorum`.
        pub w_quorum: u64,
        /// `g.quorum`.
        pub g_quorum: u64,
        /// `g.dmn`.
        pub g_dmn: u64,
        /// `w.mnhf`.
        pub w_mnhf: u64,
        /// `g.mnhf`.
        pub g_mnhf: u64,
    }
}

#[cfg(feature = "test-support")]
mod counting {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, Weak};

    use dash_consensus::coins::CoinsView;
    use dash_consensus::primitives::canonical::TRANSACTION_COINBASE;
    use dash_consensus::{Block, Tx, TxValidationError};
    use dash_dml::specialtx::{BlockPayloads, cbtx_payload, shared_collateral_template_outputs};
    use dash_primitives::CoinbaseCommitment;
    use dash_types::Numeric as _;

    use super::super::ev1::{DmlChecks, Ev1Checks, SpecialTxCtx};
    use super::GateCounts;
    use crate::chain::Fatal;
    use crate::connect::SpecialTxFailure;
    use crate::store::CoinsReadError;

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
        fn check_tx<'a>(
            &self,
            block: &Block,
            payloads: &BlockPayloads<'_>,
            i: usize,
            cx: SpecialTxCtx<'a>,
            quorum: Option<dash_dml::specialtx::QuorumSeams<'a>>,
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
            self.inner.check_tx(block, payloads, i, cx, quorum)
        }

        fn shared_collateral_spends(
            &self,
            tx: &Tx,
            view: &dyn CoinsView<Error = CoinsReadError>,
        ) -> Result<Result<(), TxValidationError>, Fatal> {
            self.counts.bump("g.coll");
            self.inner.shared_collateral_spends(tx, view)
        }

        /// BL-564: `NodeEvo` counts its five in these cells, so they register and live with the six.
        fn gate_counts(&self) -> Option<Arc<GateCounts>> {
            Some(Arc::clone(&self.counts))
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
#[path = "gates_tests.rs"]
mod tests;
