//! Core's `GetMNPayee` and `GetProjectedMNPayees`, literally (EV2 §3.4): scans over the list in
//! Core's iteration order ([`MnList::core_order`]). Compiled in every build: the payee functions
//! fall back to them on a list where the indexed path is not provably Core's (P1 or P2 false),
//! and the reference twin uses them as they are.

use std::cmp::Ordering;
use std::sync::Arc;

use crate::ProTxHash;
use crate::list::MnList;
use crate::payee::PayeeRules;
use crate::types::{Counts, Dmn};

/// What Core's scans read of a list: [`MnList`], and the reference twin's list under xcheck.
pub(crate) trait ScanList {
    /// `GetHeight()`.
    fn height(&self) -> i32;
    /// `GetAllMNsCount() == 0`.
    fn is_empty(&self) -> bool;
    /// `ForEachMN`'s order.
    fn core_order(&self) -> Arc<[ProTxHash]>;
    /// `GetMN(proTxHash)`.
    fn get(&self, pro_tx: &ProTxHash) -> Option<&Arc<Dmn>>;
    /// `GetCounts()`.
    fn counts(&self) -> Counts;
}

impl ScanList for MnList {
    fn height(&self) -> i32 {
        MnList::height(self)
    }

    fn is_empty(&self) -> bool {
        MnList::is_empty(self)
    }

    fn core_order(&self) -> Arc<[ProTxHash]> {
        MnList::core_order(self)
    }

    fn get(&self, pro_tx: &ProTxHash) -> Option<&Arc<Dmn>> {
        MnList::get(self, pro_tx)
    }

    fn counts(&self) -> Counts {
        MnList::counts(self)
    }
}

/// `dmn_types::Evo.voting_weight`: an Evo masternode is paid this many blocks in a row before
/// MN_RR.
pub(crate) const EVO_VOTING_WEIGHT: i32 = 4;

/// `CompareByLastPaid(a, b)`: a strict total order, `CompareByLastPaid_GetHeight` then memcmp.
// Core: evo/deterministicmns.cpp:143 CompareByLastPaid
pub(crate) fn compare_by_last_paid(a: &Dmn, b: &Dmn) -> Ordering {
    a.payee_key().cmp(&b.payee_key())
}

/// Whether `dmn` continues an Evo payment streak at `height` (the first scans of `GetMNPayee`
/// and `GetProjectedMNPayees`).
pub(crate) fn continues_streak(dmn: &Dmn, height: i32) -> bool {
    dmn.hot.last_paid_height == height
        && dmn.is_evo()
        && dmn.hot.consecutive_payments < EVO_VOTING_WEIGHT
}

/// The length of a projection: `count`, at most the weighted count of valid masternodes (the
/// enabled count from MN_RR).
pub(crate) fn projection_len(list: &impl ScanList, rules: PayeeRules, count: usize) -> usize {
    let counts = list.counts();
    let weighted = if rules.mn_rr {
        counts.enabled()
    } else {
        counts.valid_weighted
    };
    // `int(weighted_count)`: the low 32 bits.
    count.min(usize::try_from(weighted as i32).unwrap_or(0))
}

/// `voting_weight - nConsecutivePayments` of a continuing evonode, in `i64` (Core's reader
/// accepts any streak, and `4 - INT_MIN` overflows `int`); callers cap the run at the
/// projection's length (D6).
pub(crate) fn remaining_payments(dmn: &Dmn) -> usize {
    let remaining = i64::from(EVO_VOTING_WEIGHT) - i64::from(dmn.hot.consecutive_payments);
    usize::try_from(remaining.max(0)).unwrap_or(usize::MAX)
}

/// `ForEachMNShared(onlyValid = true, …)`: the valid masternodes in Core's order.
fn valid_in_core_order<L: ScanList>(list: &L) -> impl Iterator<Item = &Arc<Dmn>> {
    let order = list.core_order();
    (0..order.len()).filter_map(move |i| list.get(&order[i]).filter(|dmn| dmn.is_valid()))
}

/// `GetMNPayee(pindexPrev)`, line by line: the evonode continuation keeps the **last** match in
/// Core's order, then the linear minimum by `CompareByLastPaid`.
// Core: evo/deterministicmns.cpp:158 CDeterministicMNList::GetMNPayee
pub(crate) fn payee_scan<L: ScanList>(list: &L, rules: PayeeRules) -> Option<&Arc<Dmn>> {
    if list.is_empty() {
        return None;
    }
    let height = list.height();
    let mut best: Option<&Arc<Dmn>> = None;
    if rules.v19 && !rules.mn_rr {
        for dmn in valid_in_core_order(list) {
            if continues_streak(dmn, height) {
                best = Some(dmn);
            }
        }
        if best.is_some() {
            return best;
        }
    }
    for dmn in valid_in_core_order(list) {
        if best.is_none_or(|b| compare_by_last_paid(dmn, b) == Ordering::Less) {
            best = Some(dmn);
        }
    }
    best
}

/// `GetProjectedMNPayees(pindexPrev, nCount)`, line by line (`nCount < 0` is the caller's
/// argument check). Before MN_RR the first scan appends the remaining payments of **every**
/// continuing evonode it meets while only the last one is skipped by the weighted scan; the sort
/// starts after the last one's remaining payments.
///
/// Every run of copies is capped at `count`, and so is the sort's offset (D6): Core's first
/// `count` entries are unchanged (a prefix of at most `count` entries of the first scan's runs
/// does not change when each run is capped at `count`, and every masternode keeps at least
/// `count - offset` copies in the sorted part, all its share of the smallest entries), while a
/// streak far below zero, which Core's reader accepts, cannot make the vector huge.
// Core: evo/deterministicmns.cpp:196 CDeterministicMNList::GetProjectedMNPayees
pub(crate) fn projected_scan<L: ScanList>(
    list: &L,
    rules: PayeeRules,
    count: usize,
) -> Vec<Arc<Dmn>> {
    let count = projection_len(list, rules, count);
    let height = list.height();
    let mut result: Vec<Arc<Dmn>> = Vec::new();
    let mut remaining = 0usize;
    let mut skipped: Option<&Arc<Dmn>> = None;
    if !rules.mn_rr {
        for dmn in valid_in_core_order(list) {
            if continues_streak(dmn, height) {
                remaining = remaining_payments(dmn).min(count);
                result.extend(std::iter::repeat_n(Arc::clone(dmn), remaining));
                skipped = Some(dmn);
            }
        }
    }
    for dmn in valid_in_core_order(list) {
        if skipped.is_some_and(|s| Arc::ptr_eq(s, dmn)) {
            continue;
        }
        let copies = if rules.mn_rr {
            1
        } else {
            dmn.weight() as usize
        };
        result.extend(std::iter::repeat_n(Arc::clone(dmn), copies));
    }
    if let Some(skipped) = skipped {
        // `util::irange(n)` is empty for n <= 0.
        let paid = usize::try_from(skipped.hot.consecutive_payments).unwrap_or(0).min(count);
        result.extend(std::iter::repeat_n(Arc::clone(skipped), paid));
    }
    // `remaining` is at most the length: the first scan put that many entries at the front.
    result[remaining..].sort_by(|a, b| compare_by_last_paid(a, b));
    // `resize(nCount)` never grows here: the entries are at least the weighted count.
    result.truncate(count);
    result
}
