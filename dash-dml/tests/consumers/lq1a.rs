//! LQ1a/b (member selection, scans) against EV0. The list views are EV0's (LQ1 §2.2, EV addendum
//! §1.3); LQ1's snapshot-row view (LQ1 §2.3) is copied here in shape, over one attempt-owned
//! overlay (EV addendum §2.6). Mined commitments are EV0's single reader trait.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::{BlockId, ChainAncestry};
use dash_dml::testkit::{MemChain, dmn, pro_tx};
use dash_dml::{Dmn, MnList, MnType, ProTxHash, UniqueKey};
use dash_dml::{EvoStore, StoreError};
use dash_evo::llmq::{
    AddMnRejected, MemberError, MnEntryView, MnListView, QuorumMemberSource, QuorumMembers,
    QuorumSnapshot, ScratchMnList,
};
use dash_evo::{Effect, EffectSink};
use dash_primitives::{BlockHash, LlmqType, QuorumHash};
use dash_types::Numeric;

// ---- LQ1 §2.2: what LQ1 needs from the list -------------------------------------------------
//
// The views are EV0's (`dash_evo::llmq`), and the `Dmn`/`MnList` impls are `dash_dml`'s
// (`Entry = Arc<Dmn>` through the forwarding impl for `Arc<T>`); this crate could not write them
// (E0117).

/// LQ1's lean scratch set (B-m1). Its `try_add` reproduces `MnList::try_add`'s failure order from
/// EV2a's `ColdDerived::unique_keys`; this stub probes the proTxHash, internal id and collateral
/// only, which is what EV0's constructors can build.
#[derive(Default)]
pub struct UsedSet {
    entries: BTreeMap<ProTxHash, Arc<Dmn>>,
    internal_ids: BTreeMap<u64, ProTxHash>,
    unique: BTreeMap<UniqueKey, ProTxHash>,
}

impl ScratchMnList for UsedSet {
    type Entry = Arc<Dmn>;
    fn try_add(&mut self, e: &Arc<Dmn>) -> Result<(), AddMnRejected> {
        let collateral = UniqueKey::collateral(&e.collateral);
        if self.entries.contains_key(&e.pro_tx_hash)
            || self.internal_ids.contains_key(&e.internal_id)
            || self.unique.contains_key(&collateral)
        {
            return Err(AddMnRejected);
        }
        self.entries.insert(e.pro_tx_hash, Arc::clone(e));
        self.internal_ids.insert(e.internal_id, e.pro_tx_hash);
        self.unique.insert(collateral, e.pro_tx_hash);
        Ok(())
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.entries.contains_key(p)
    }
    fn total(&self) -> usize {
        self.entries.len()
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Arc<Dmn>)) {
        for dmn in self.entries.values().filter(|dmn| !only_valid || dmn.is_valid()) {
            f(dmn);
        }
    }
}

// ---- LQ1 §2.5: mined commitments ---------------------------------------------------------------
//
// LQ1's `MinedCommitmentIndex` is retired (EV0 API review Q1): LQ1a reads EV0's
// `MinedCommitmentReader` directly, and needs only the commitment's `quorum_index`.

/// The newest commitment's quorum index per scanned base, as LQ1's scan reads it.
pub fn newest_indexes(
    store: &dyn EvoStore,
    t: LlmqType,
    at: BlockId,
    max: usize,
) -> Result<Vec<(BlockId, Option<i16>)>, StoreError> {
    let mut out = Vec::new();
    for base in store.mined_until(t, at, max)? {
        let quorum_hash = QuorumHash::from(*store.chain().header(base).hash.as_bytes());
        let index =
            store.mined_commitment(t, &quorum_hash, at)?.and_then(|(qc, _)| qc.quorum_index);
        out.push((base, index));
    }
    Ok(out)
}

// ---- LQ1 §2.3, §3.7.2; EV addendum §2.6: snapshot rows, attempt-scoped -----------------------

pub trait SnapshotRows {
    type Error;
    fn get(
        &self,
        t: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, Self::Error>;
    fn store(
        &mut self,
        t: LlmqType,
        cycle_base: &BlockHash,
        s: &QuorumSnapshot,
    ) -> Result<(), Self::Error>;
}

/// One attempt's rows, keyed as the store keys them (the type's base byte); the last write wins.
pub type Overlay = RefCell<BTreeMap<(u8, BlockHash), QuorumSnapshot>>;

/// One call's `SnapshotRows` (EV addendum §2.6): a view over its attempt's overlay. A write goes to
/// the overlay and is an effect in the attempt's one log (it survives the attempt's failure, as
/// Core's raw write does); a read sees the overlay, then the backing rows.
pub struct AttemptRows<'a> {
    pub overlay: &'a Overlay,
    pub backing: &'a dyn EvoStore,
    pub sink: &'a mut dyn EffectSink,
}

impl SnapshotRows for AttemptRows<'_> {
    type Error = StoreError;
    fn get(
        &self,
        t: LlmqType,
        cycle_base: &BlockHash,
    ) -> Result<Option<QuorumSnapshot>, StoreError> {
        if let Some(s) = self.overlay.borrow().get(&(t.to_base(), *cycle_base)) {
            return Ok(Some(s.clone()));
        }
        Ok(self.backing.quorum_snapshot(t, cycle_base)?.map(|s| (*s).clone()))
    }
    fn store(
        &mut self,
        t: LlmqType,
        cycle_base: &BlockHash,
        s: &QuorumSnapshot,
    ) -> Result<(), StoreError> {
        self.sink.emit(Effect::StoreQuorumSnapshot {
            llmq_type: t,
            cycle_base: *cycle_base,
            snapshot: s.clone(),
        });
        self.overlay.borrow_mut().insert((t.to_base(), *cycle_base), s.clone());
        Ok(())
    }
}

/// CH7b's `AttemptHandle` in shape (EV addendum §2.6; LQ1m's `ModelAttempt` models it): it owns
/// the attempt's overlay, and each member call builds its `AttemptRows` over that one overlay.
pub struct AttemptHandle<'a> {
    backing: &'a dyn EvoStore,
    roster: Vec<Arc<Dmn>>,
    overlay: Overlay,
}

impl<'a> AttemptHandle<'a> {
    pub fn begin(backing: &'a dyn EvoStore, roster: Vec<Arc<Dmn>>) -> Self {
        AttemptHandle {
            backing,
            roster,
            overlay: Overlay::default(),
        }
    }
    /// One call's rows over the attempt's overlay.
    pub fn rows<'s>(&'s self, sink: &'s mut dyn EffectSink) -> AttemptRows<'s> {
        AttemptRows {
            overlay: &self.overlay,
            backing: self.backing,
            sink,
        }
    }
}

impl QuorumMemberSource for AttemptHandle<'_> {
    type Entry = Arc<Dmn>;
    type Error = StoreError;
    fn members(
        &self,
        t: LlmqType,
        at: BlockId,
        effects: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        rotate_one(&mut self.rows(effects), self.backing.chain(), &self.roster, t, at)
    }
}

/// A stub rotated computation over rows: the parent's row marks the roster entry used there, this
/// block uses the next one and stores its own row. As in Core (EV addendum §2.1), an absent row
/// is a value, not an error: it breaks the rotation, which restarts at the first entry; genesis
/// has no members.
fn rotate_one(
    rows: &mut dyn SnapshotRows<Error = StoreError>,
    chain: &dyn ChainAncestry,
    roster: &[Arc<Dmn>],
    t: LlmqType,
    at: BlockId,
) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
    let Some(parent) = chain.prev(at) else {
        return Ok(Vec::new().into());
    };
    let used = rows.get(t, &chain.header(parent).hash).map_err(MemberError::View)?;
    let next = match used.and_then(|s| s.active_quorum_members.iter().position(|&a| a)) {
        Some(last) => (last + 1) % roster.len(),
        None => 0,
    };
    let mut active = vec![false; roster.len()];
    active[next] = true;
    let row = QuorumSnapshot {
        active_quorum_members: active,
        ..Default::default()
    };
    rows.store(t, &chain.header(at).hash, &row).map_err(MemberError::View)?;
    Ok(vec![Arc::clone(&roster[next])].into())
}

// ---- A member source for tests (LQ1's reference model stands here) -----------------------------

/// Returns fixed members, or a fixed error.
pub struct FixedMembers(Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>>);

impl FixedMembers {
    pub fn new(members: Vec<Arc<Dmn>>) -> Self {
        FixedMembers(Ok(members.into()))
    }
    pub fn failing(e: MemberError<StoreError>) -> Self {
        FixedMembers(Err(e))
    }
}

impl QuorumMemberSource for FixedMembers {
    type Entry = Arc<Dmn>;
    type Error = StoreError;
    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn EffectSink,
    ) -> Result<QuorumMembers<Arc<Dmn>>, MemberError<StoreError>> {
        self.0.clone()
    }
}

#[test]
fn an_mn_list_is_an_lq1_list_view() {
    let list = MnList::default_pre_dip3();
    let mut seen = 0;
    MnListView::for_each(&list, true, &mut |_| seen += 1);
    assert_eq!((seen, MnListView::counts(&list).total), (0, 0));
    assert_eq!(list.new_scratch().total(), 0);
    let mut scratch = UsedSet::default();
    let entry = dmn(1, MnType::Regular);
    assert!(scratch.try_add(&entry).is_ok());
    assert!(scratch.try_add(&entry).is_err(), "AddMN rejects a duplicate proTxHash");
    assert!(entry.confirmed_hash_is_null());
}

/// A chain genesis → b1 → b2, and a store holding only genesis's row (roster entry 0 used).
pub fn rotation_fixture() -> (MemChain, BlockId, BlockId, dash_dml::MemEvoStore, Vec<Arc<Dmn>>) {
    let mut chain = MemChain::new();
    let b1 = chain.child(chain.genesis());
    let b2 = chain.child(b1);
    let mut store = dash_dml::MemEvoStore::new();
    let genesis_row = QuorumSnapshot {
        active_quorum_members: vec![true, false, false],
        ..Default::default()
    };
    store.store_quorum_snapshot(T, chain.hash(chain.genesis()), genesis_row);
    let roster = (1..=3).map(|n| dmn(n, MnType::Regular)).collect();
    (chain, b1, b2, store, roster)
}

pub const T: LlmqType = LlmqType::LlmqTestDip0024;

/// EV addendum §2.6 case 1: call 1 (b1, on `first`) stores a row, call 2 (b2, on `second`) reads
/// the row for b1. Both store, so the log holds both rows in call order either way.
fn two_calls(
    first: &AttemptHandle<'_>,
    second: &AttemptHandle<'_>,
    chain: &MemChain,
    b1: BlockId,
    b2: BlockId,
) -> Vec<ProTxHash> {
    let mut log: Vec<Effect> = Vec::new();
    let first = first.members(T, b1, &mut log).unwrap();
    let second = second.members(T, b2, &mut log).unwrap();
    let cycle_bases: Vec<BlockHash> = log
        .iter()
        .map(|e| match e {
            Effect::StoreQuorumSnapshot {
                cycle_base,
                ..
            } => *cycle_base,
            other => panic!("unexpected effect {other:?}"),
        })
        .collect();
    assert_eq!(cycle_bases, [chain.hash(b1), chain.hash(b2)], "one ordered log for both calls");
    first.iter().chain(second.iter()).map(|e| *e.pro_tx_hash()).collect()
}

#[test]
fn a_later_member_call_reads_the_row_an_earlier_one_stored() {
    let (chain, b1, b2, store, roster) = rotation_fixture();
    let view = store.view(&chain);
    let handle = AttemptHandle::begin(&view, roster.clone());
    assert_eq!(two_calls(&handle, &handle, &chain, b1, b2), [pro_tx(2), pro_tx(3)]);

    // Negative control, EV0's former per-call overlay: call 2 cannot see call 1's unapplied row,
    // reads Core's absence and restarts the rotation, so the members differ.
    let call_1 = AttemptHandle::begin(&view, roster.clone());
    let call_2 = AttemptHandle::begin(&view, roster);
    assert_eq!(two_calls(&call_1, &call_2, &chain, b1, b2), [pro_tx(2), pro_tx(1)]);
}

#[test]
fn a_later_store_overwrites_and_both_are_effects_in_order() {
    let (chain, _, _, store, roster) = rotation_fixture();
    let view = store.view(&chain);
    let handle = AttemptHandle::begin(&view, roster);
    let base = chain.hash(chain.genesis());
    let row = |bits: [bool; 3]| QuorumSnapshot {
        active_quorum_members: bits.to_vec(),
        ..Default::default()
    };
    let mut log: Vec<Effect> = Vec::new();
    // Two stores to one key, from two calls of the attempt; a third call reads.
    for bits in [[false, true, false], [false, false, true]] {
        handle.rows(&mut log).store(T, &base, &row(bits)).unwrap();
    }
    assert_eq!(
        handle.rows(&mut Vec::new()).get(T, &base).unwrap(),
        Some(row([false, false, true]))
    );
    assert_eq!(
        view.quorum_snapshot(T, &base).unwrap().as_deref(),
        Some(&row([true, false, false]))
    );
    let effect = |bits| Effect::StoreQuorumSnapshot {
        llmq_type: T,
        cycle_base: base,
        snapshot: row(bits),
    };
    assert_eq!(log, [effect([false, true, false]), effect([false, false, true])]);
    assert!(newest_indexes(&view, T, chain.genesis(), 4).unwrap().is_empty());
}
