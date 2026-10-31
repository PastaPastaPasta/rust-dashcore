//! LQ1's list views over the DML (LQ1 §2.2.4; EV addendum §1.3, §3 commit 1, Opus r7 O2), and
//! the operator key SB8's commitment check reads.
//!
//! `Dmn` implements the entry view, so `Entry = Arc<Dmn>` comes through the forwarding impl for
//! `Arc<T>`: unchanged masternodes keep cheap clones at the algorithm boundary. Field reads only.
//! The rotation's used sets are the lean [`UsedSet`] (LQ1b-E), with `MnList` itself as its twin.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_evo::llmq::{
    AddMnRejected, ListCounts, MnEntryView, MnListView, OperatorKeyView, ScratchMnList,
};
use dash_pkc::bls::core::BlsPkValue;
use dash_primitives::OutPoint;

use crate::ProTxHash;
use crate::list::{MnList, add_props, in_state_domain};
use crate::types::Dmn;
use crate::unique::UniqueMap;

impl MnEntryView for Dmn {
    fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }
    fn collateral(&self) -> &OutPoint {
        &self.collateral
    }
    fn is_evo(&self) -> bool {
        Dmn::is_evo(self)
    }
    fn is_banned(&self) -> bool {
        self.hot.is_banned()
    }
    fn confirmed_hash_is_null(&self) -> bool {
        self.cold.confirmed_hash.is_null()
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        self.cold.confirmed_hash_with_pro_tx.as_bytes()
    }
}

/// The key a commitment's `membersSig` is checked against (SB8).
impl OperatorKeyView for Dmn {
    /// `pdmnState->pubKeyOperator.Get()`, read from this entry's own state (LQ1 B-m2).
    fn operator_key(&self) -> BlsPkValue {
        self.cold.pub_key_operator.get()
    }
}

/// `ForEachMNShared(onlyValid, …)` over entries.
fn for_each_entry<'a>(
    entries: impl Iterator<Item = &'a Arc<Dmn>>,
    only_valid: bool,
    f: &mut dyn FnMut(&Arc<Dmn>),
) {
    for dmn in entries.filter(|dmn| !only_valid || dmn.is_valid()) {
        f(dmn);
    }
}

impl MnListView for MnList {
    type Entry = Arc<Dmn>;
    type Scratch = UsedSet;

    fn counts(&self) -> ListCounts {
        let c = MnList::counts(self);
        ListCounts {
            total: c.total() as usize,
            enabled: c.enabled() as usize,
        }
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.get(p).is_some()
    }
    fn is_pose_banned(&self, p: &ProTxHash) -> bool {
        MnList::is_pose_banned(self, p)
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Arc<Dmn>)) {
        for_each_entry(self.iter(), only_valid, f);
    }
    fn new_scratch(&self) -> UsedSet {
        UsedSet::default()
    }
}

/// A rotation's `MnsUsedAtH` set (LQ1 §2.2.4, B-m1): what Core's `AddMN` reads and nothing else.
///
/// It holds the masternodes, their internal ids and the unique-property map, and accepts exactly
/// what [`MnList::try_add`] accepts, through the same steps in the same order (EV2 §2.5 steps 1-7:
/// the state domain, proTxHash, internal id, then [`add_props`], built from
/// `ColdDerived::unique_keys`). It keeps none of a list's indexes (payee order, decaying,
/// unconfirmed, streaks, `core_order`) and no registration counter: `total()` is the count, as
/// Core's `GetCounts().total()`. Its twin is `MnList::scratch()` with `MnList::try_add(_, true)`,
/// compared call by call in this module's tests.
#[derive(Clone, Default)]
pub struct UsedSet {
    mns: BTreeMap<ProTxHash, Arc<Dmn>>,
    internal_ids: BTreeSet<u64>,
    unique: UniqueMap,
}

impl ScratchMnList for UsedSet {
    type Entry = Arc<Dmn>;

    /// Core's `AddMN(dmn, fBumpTotalCount = true)`, all or nothing.
    // Core: evo/deterministicmns.cpp:439 CDeterministicMNList::AddMN
    fn try_add(&mut self, e: &Arc<Dmn>) -> Result<(), AddMnRejected> {
        in_state_domain(&e.pro_tx_hash, &e.cold).map_err(|_| AddMnRejected)?;
        if self.mns.contains_key(&e.pro_tx_hash) || self.internal_ids.contains(&e.internal_id) {
            return Err(AddMnRejected);
        }
        // An `imbl` map: the clone is O(1), and a failure leaves the set unchanged.
        let mut unique = self.unique.clone();
        add_props(&mut unique, e).map_err(|_| AddMnRejected)?;
        self.unique = unique;
        self.internal_ids.insert(e.internal_id);
        self.mns.insert(e.pro_tx_hash, Arc::clone(e));
        Ok(())
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.mns.contains_key(p)
    }
    fn total(&self) -> usize {
        self.mns.len()
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Arc<Dmn>)) {
        for_each_entry(self.mns.values(), only_valid, f);
    }
}

/// `MnList::scratch()` filled with Core's `AddMN(dmn, fBumpTotalCount = true)`: [`UsedSet`]'s
/// twin (LQ1 §2.2.4).
impl ScratchMnList for MnList {
    type Entry = Arc<Dmn>;

    /// Core's `AddMN` with `fBumpTotalCount = true`: [`MnList::try_add`] (EV2 §2.5).
    fn try_add(&mut self, e: &Arc<Dmn>) -> Result<(), AddMnRejected> {
        MnList::try_add(self, Arc::clone(e), true).map_err(|_| AddMnRejected)
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.get(p).is_some()
    }
    fn total(&self) -> usize {
        MnList::counts(self).total() as usize
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Arc<Dmn>)) {
        for_each_entry(self.iter(), only_valid, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MnType;
    use crate::testkit::{dmn, dmn_with, numbered, operator_key, pro_tx};
    use dash_pkc::bls::BlsSchemeKind;

    /// The forwarding impl: an `Arc<Dmn>` is an entry, and it reads the `Dmn`'s fields.
    #[test]
    fn an_arc_dmn_is_an_entry() {
        fn read<E: MnEntryView>(e: &E) -> (ProTxHash, OutPoint, bool, bool, bool, [u8; 32]) {
            (
                *e.pro_tx_hash(),
                *e.collateral(),
                e.is_evo(),
                e.is_banned(),
                e.confirmed_hash_is_null(),
                *e.confirmed_hash_with_protx(),
            )
        }
        let entry = dmn(3, MnType::Evo);
        let fields = read(&entry);
        assert_eq!(fields, read(&*entry));
        assert_eq!(fields, (entry.pro_tx_hash, entry.collateral, true, false, true, [0u8; 32]));
    }

    #[test]
    fn an_mn_list_is_an_lq1_list_view() {
        let mut list = MnList::default_pre_dip3();
        list.insert_for_tests(dmn(1, MnType::Regular));
        let mut seen = Vec::new();
        MnListView::for_each(&list, true, &mut |e| seen.push(*e.pro_tx_hash()));
        assert_eq!(seen, [pro_tx(1)]);
        assert!(MnListView::contains(&list, &pro_tx(1)));
        assert!(!MnListView::is_pose_banned(&list, &pro_tx(2)));
        let scratch = list.new_scratch();
        assert_eq!((scratch.total(), ScratchMnList::contains(&scratch, &pro_tx(1))), (0, false));
    }

    /// Candidates that collide on each of `AddMN`'s checks, in pairs and chains.
    fn candidates() -> Vec<Arc<Dmn>> {
        let mut out: Vec<Arc<Dmn>> = (1..=4).map(|n| dmn(n, MnType::Regular)).collect();
        out.push(dmn(5, MnType::Evo));
        // proTxHash 1 again, with every other property fresh.
        out.push(dmn_with(11, MnType::Regular, |d, _| d.pro_tx_hash = pro_tx(1)));
        // Internal id 2 again.
        out.push(dmn_with(12, MnType::Regular, |d, _| d.internal_id = 2));
        // Collateral 3 again.
        out.push(dmn_with(13, MnType::Regular, |d, _| {
            d.collateral = dmn(3, MnType::Regular).collateral
        }));
        // Owner 4 again, and an operator key shared by two fresh ones.
        out.push(dmn_with(14, MnType::Regular, |_, c| {
            c.key_id_owner = dmn(4, MnType::Regular).cold.key_id_owner
        }));
        for n in [15, 16] {
            out.push(dmn_with(n, MnType::Regular, |_, c| {
                c.version = 2;
                c.pub_key_operator = operator_key(9, BlsSchemeKind::Basic);
            }));
        }
        // Evo masternodes sharing a platformNodeID.
        for n in [17, 18] {
            out.push(dmn_with(n, MnType::Evo, |_, c| c.platform_node_id = [0x77; 20].into()));
        }
        // Mainnet-shaped ones: services, payouts and operator keys.
        out.extend((100..104).map(numbered));
        // Outside the state domain: a key under the other scheme from its version's.
        out.push(dmn_with(19, MnType::Regular, |_, c| {
            c.version = 1;
            c.pub_key_operator = operator_key(19, BlsSchemeKind::Basic);
        }));
        out
    }

    /// `UsedSet` is `MnList::scratch()` + `try_add(_, true)` call by call: the same accept or
    /// reject, the same members, counts and valid entries after every call.
    #[test]
    fn the_used_set_is_its_twin() {
        let pool = candidates();
        let mut seed = 0x28u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize
        };
        let (mut accepted, mut rejected) = (0, 0);
        for round in 0..400 {
            let mut used = UsedSet::default();
            let mut twin = MnList::scratch();
            let len = 1 + next() % 12;
            for _ in 0..len {
                let e = &pool[next() % pool.len()];
                let ours = ScratchMnList::try_add(&mut used, e);
                let theirs = ScratchMnList::try_add(&mut twin, e);
                assert_eq!(ours, theirs, "round {round}: {:?}", e.pro_tx_hash);
                if ours.is_ok() {
                    accepted += 1;
                } else {
                    rejected += 1;
                }
                assert_eq!(ScratchMnList::total(&used), ScratchMnList::total(&twin));
                for only_valid in [false, true] {
                    let (mut a, mut b) = (Vec::new(), Vec::new());
                    ScratchMnList::for_each(&used, only_valid, &mut |e| a.push(Arc::clone(e)));
                    ScratchMnList::for_each(&twin, only_valid, &mut |e| b.push(Arc::clone(e)));
                    a.sort_by_key(|e| e.pro_tx_hash);
                    b.sort_by_key(|e| e.pro_tx_hash);
                    assert!(a.iter().zip(&b).all(|(x, y)| Arc::ptr_eq(x, y)) && a.len() == b.len());
                }
                for c in &pool {
                    assert_eq!(
                        ScratchMnList::contains(&used, &c.pro_tx_hash),
                        ScratchMnList::contains(&twin, &c.pro_tx_hash)
                    );
                }
            }
        }
        assert!(accepted > 400 && rejected > 400, "non-vacuous: {accepted} / {rejected}");
    }
}
