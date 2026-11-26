//! The exact-block memo of a template's list build (A1 §2.13): ours, not Core's. Core's
//! `CreateNewBlock` builds the list with its `CMNListBuilder`, then `TestBlockValidity` builds it
//! again in `ProcessSpecialTxsInBlock`. The template hands its successful build to that second
//! build of the same block, at the same tip, which uses it in place of the build and nothing else.
//!
//! A build reads of the block only its parent and the transactions after the coinbase, in order
//! (the coinbase's txid changes with its CbTx, and the build skips position 0); the rest is a
//! function of the parent at an unchanged tip. A txid is an identity, not an instance hash (a
//! version-2 asset unlock's leaves out its signature), which is sound for the list only: the memo
//! stands for no check, signature, credit pool or verdict.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dash_consensus::primitives::Block;
use dash_primitives::{BlockHash, TxHash};

use super::{BuiltList, DmlRules};
use crate::list::MnList;
use crate::netinfo::NetInfoRules;

/// What a list build reads of its block and its rules: the parent's hash, the txids after the
/// coinbase in block order, and every [`DmlRules`] field that follows from the parent or the node
/// (V19, MN_RR, V24, the NetInfo rules). The [`DmlParams`](super::DmlParams) are the chain's
/// constants, fixed for the process, so the key leaves them out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmlMemoKey {
    prev: BlockHash,
    txids: Vec<TxHash>,
    v19: bool,
    mn_rr: bool,
    v24: bool,
    net_info: NetInfoRules,
}

impl DmlMemoKey {
    /// `block`'s key under `rules`, or `None` when its first transaction is not a coinbase.
    pub fn of(block: &Block, rules: &DmlRules<'_>) -> Option<DmlMemoKey> {
        let (first, rest) = block.txs().split_first()?;
        first.is_coinbase().then(|| DmlMemoKey {
            prev: block.header().prev_hash(),
            txids: rest.iter().map(|tx| tx.txid()).collect(),
            v19: rules.v19,
            mn_rr: rules.mn_rr,
            v24: rules.v24,
            net_info: *rules.net_info,
        })
    }
}

/// Why a memo stood for no build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmlMemoMiss {
    /// The block's key is not the memo's.
    Key,
    /// The tip the build reads is not the memo's parent.
    Tip,
    /// The template's build emitted an effect, so no memo was made.
    Effects,
}

/// The counts of one memo producer, shared with every memo it makes.
#[derive(Debug, Default)]
pub struct DmlMemoCounts {
    hits: AtomicU64,
    key: AtomicU64,
    tip: AtomicU64,
    effects: AtomicU64,
}

/// [`DmlMemoCounts`] read at once. A memo passed to an attempt that fails before the list build,
/// or below DIP0003, is counted nowhere, so hits plus misses can be fewer than the memos passed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DmlMemoTally {
    pub hits: u64,
    pub key: u64,
    pub tip: u64,
    pub effects: u64,
}

impl DmlMemoCounts {
    /// A memo stood for a build.
    pub fn hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
    }

    /// A memo stood for no build, or none was made, for `why`.
    pub fn miss(&self, why: DmlMemoMiss) {
        let counter = match why {
            DmlMemoMiss::Key => &self.key,
            DmlMemoMiss::Tip => &self.tip,
            DmlMemoMiss::Effects => &self.effects,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn tally(&self) -> DmlMemoTally {
        DmlMemoTally {
            hits: self.hits.load(Ordering::Relaxed),
            key: self.key.load(Ordering::Relaxed),
            tip: self.tip.load(Ordering::Relaxed),
            effects: self.effects.load(Ordering::Relaxed),
        }
    }
}

/// A template's successful list build of the block with key `key`, before `SetBlockHash`.
pub struct DmlMemo {
    pub key: DmlMemoKey,
    pub built: BuiltList,
    counts: Arc<DmlMemoCounts>,
    cross_check: bool,
}

impl DmlMemo {
    /// The memo of `built`, counted on `counts`, cross-checked: a test-support consumer also
    /// builds the list on each hit and asserts the two agree (others ignore the flag).
    pub fn new(key: DmlMemoKey, built: BuiltList, counts: Arc<DmlMemoCounts>) -> DmlMemo {
        DmlMemo {
            key,
            built,
            counts,
            cross_check: true,
        }
    }

    pub fn counts(&self) -> &DmlMemoCounts {
        &self.counts
    }

    /// The memo's list for the build of `block` under `rules` over coins whose best block is
    /// `tip`, counted as a hit; `None` on a miss, counted by its reason.
    pub fn list_for(
        &self,
        block: &Block,
        rules: &DmlRules<'_>,
        tip: Option<BlockHash>,
    ) -> Option<&MnList> {
        let why = if DmlMemoKey::of(block, rules).as_ref() != Some(&self.key) {
            DmlMemoMiss::Key
        } else if tip != Some(self.key.prev) {
            DmlMemoMiss::Tip
        } else {
            self.counts.hit();
            return Some(&self.built.list);
        };
        self.counts.miss(why);
        None
    }

    /// This memo without the cross-check, for a test that passes a list other than the build's.
    pub fn without_cross_check(mut self) -> DmlMemo {
        self.cross_check = false;
        self
    }

    pub fn cross_check(&self) -> bool {
        self.cross_check
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::primitives::{Header, Transaction, TxIn, TxOut, TxRef};
    use dash_primitives::{BlockHeader, OutPoint, TxType};

    use super::*;
    use crate::DmlParams;
    use crate::netinfo::NetInfoConfig;
    use crate::testkit::replay::mainnet;

    fn tx(n: u8, coinbase: bool) -> TxRef {
        let prevout = if coinbase {
            OutPoint::new(TxHash::from([0; 32]), u32::MAX)
        } else {
            OutPoint::new(TxHash::from([n; 32]), 0)
        };
        let input = TxIn::new(prevout, vec![n].into(), u32::MAX);
        let output = TxOut::from_duffs(i64::from(n), Vec::new().into());
        let raw = Transaction::new(1, TxType::Spend, vec![input], vec![output], 0, Vec::new());
        Arc::new(dash_consensus::Tx::new(raw))
    }

    fn block(prev: u8, txs: Vec<TxRef>) -> Block {
        let base = BlockHeader::default();
        let header = BlockHeader::new(
            base.version(),
            BlockHash::from([prev; 32]),
            base.merkle_root(),
            base.time(),
            base.bits(),
            base.nonce(),
        );
        Block::new(Header::new(header), txs)
    }

    fn net_info(cjdns_reachable: bool) -> NetInfoRules {
        NetInfoRules::new(
            mainnet(),
            mainnet(),
            NetInfoConfig {
                cjdns_reachable,
            },
        )
    }

    fn rules<'a>(params: &'a DmlParams, net_info: &'a NetInfoRules) -> DmlRules<'a> {
        DmlRules {
            params,
            net_info,
            v19: true,
            mn_rr: true,
            v24: true,
        }
    }

    fn memo(b: &Block, rules: &DmlRules<'_>, counts: &Arc<DmlMemoCounts>) -> DmlMemo {
        let key = DmlMemoKey::of(b, rules).expect("a coinbase");
        let built = BuiltList {
            list: MnList::scratch(),
            payee: None,
        };
        DmlMemo::new(key, built, Arc::clone(counts))
    }

    #[test]
    fn the_key_is_the_parent_the_txids_after_the_coinbase_in_order_and_the_rules() {
        let (dml, ni) = (DmlParams::new(mainnet()), net_info(false));
        let r = rules(&dml, &ni);
        let b = block(1, vec![tx(0, true), tx(2, false), tx(3, false)]);
        let key = DmlMemoKey::of(&b, &r).expect("a coinbase");
        assert_eq!(key.prev, BlockHash::from([1; 32]));
        let other_coinbase = block(1, vec![tx(9, true), tx(2, false), tx(3, false)]);
        assert_eq!(DmlMemoKey::of(&other_coinbase, &r).as_ref(), Some(&key));
        let reordered = block(1, vec![tx(0, true), tx(3, false), tx(2, false)]);
        assert_ne!(DmlMemoKey::of(&reordered, &r).as_ref(), Some(&key));
        let other_prev = block(4, vec![tx(0, true), tx(2, false), tx(3, false)]);
        assert_ne!(DmlMemoKey::of(&other_prev, &r).as_ref(), Some(&key));
        let shorter = block(1, vec![tx(0, true), tx(2, false)]);
        assert_ne!(DmlMemoKey::of(&shorter, &r).as_ref(), Some(&key));
    }

    #[test]
    fn a_block_without_a_coinbase_first_has_no_key() {
        let (dml, ni) = (DmlParams::new(mainnet()), net_info(false));
        let r = rules(&dml, &ni);
        assert_eq!(DmlMemoKey::of(&block(1, vec![]), &r), None);
        assert_eq!(DmlMemoKey::of(&block(1, vec![tx(2, false), tx(0, true)]), &r), None);
    }

    #[test]
    fn a_lookup_hits_on_its_block_at_its_tip_and_counts_each_miss_by_reason() {
        let (dml, ni) = (DmlParams::new(mainnet()), net_info(false));
        let r = rules(&dml, &ni);
        let counts = Arc::new(DmlMemoCounts::default());
        let b = block(1, vec![tx(0, true), tx(2, false), tx(3, false)]);
        let m = memo(&b, &r, &counts);
        let prev = Some(BlockHash::from([1; 32]));
        assert!(m.list_for(&b, &r, prev).is_some());
        let reordered = block(1, vec![tx(0, true), tx(3, false), tx(2, false)]);
        assert!(m.list_for(&reordered, &r, prev).is_none());
        assert!(m.list_for(&b, &r, Some(BlockHash::from([5; 32]))).is_none());
        assert!(m.list_for(&b, &r, None).is_none());
        counts.miss(DmlMemoMiss::Effects);
        let want = DmlMemoTally {
            hits: 1,
            key: 1,
            tip: 2,
            effects: 1,
        };
        assert_eq!(counts.tally(), want);
    }

    #[test]
    fn a_memo_made_under_other_rules_misses_on_its_key() {
        let (dml, ni, cjdns) = (DmlParams::new(mainnet()), net_info(false), net_info(true));
        let r = rules(&dml, &ni);
        let others = [
            (
                "v19",
                DmlRules {
                    v19: false,
                    ..r
                },
            ),
            (
                "mn_rr",
                DmlRules {
                    mn_rr: false,
                    ..r
                },
            ),
            (
                "v24",
                DmlRules {
                    v24: false,
                    ..r
                },
            ),
            (
                "net_info",
                DmlRules {
                    net_info: &cjdns,
                    ..r
                },
            ),
        ];
        let b = block(1, vec![tx(0, true), tx(2, false), tx(3, false)]);
        let prev = Some(BlockHash::from([1; 32]));
        for (field, other) in others {
            let counts = Arc::new(DmlMemoCounts::default());
            let m = memo(&b, &other, &counts);
            assert!(m.list_for(&b, &r, prev).is_none(), "{field}: no hit");
            let want = DmlMemoTally {
                key: 1,
                ..DmlMemoTally::default()
            };
            assert_eq!(counts.tally(), want, "{field}: a key miss");
            assert!(m.list_for(&b, &other, prev).is_some(), "{field}: a hit under its own rules");
        }
    }

    #[test]
    fn a_memo_is_cross_checked_unless_a_test_opts_out() {
        let (dml, ni) = (DmlParams::new(mainnet()), net_info(false));
        let r = rules(&dml, &ni);
        let counts = Arc::new(DmlMemoCounts::default());
        let m = memo(&block(1, vec![tx(0, true)]), &r, &counts);
        assert!(m.cross_check());
        assert!(!m.without_cross_check().cross_check());
    }
}
