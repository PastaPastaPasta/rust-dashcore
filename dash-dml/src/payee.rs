//! Payee selection and projections (EV2 §3.4; FR-EVO-04; quirk 13).
//!
//! Two paths, one contract: [`MnList::payee`] and [`MnList::projected_payees`] equal Core's
//! literal algorithm on every list Core's canonical reader accepts. The indexed path walks the
//! payee queue; it runs only when the list itself satisfies P1 (at most one valid masternode paid
//! at the list height) and P2 (a positive height, or no negative registration height), checked
//! lazily and never inherited (rev3). Otherwise the literal scans of [`crate::scan`] run.

use std::sync::Arc;

use crate::list::MnList;
use crate::scan::{self, continues_streak, projection_len, remaining_payments};
use crate::types::Dmn;

/// The deployments `GetMNPayee` and `GetProjectedMNPayees` read:
/// `DeploymentActiveAfter(pindexPrev, V19 / MN_RR)`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PayeeRules {
    /// V19: evonodes exist and continue their streaks (until MN_RR).
    pub v19: bool,
    /// MN_RR: no continuation, every masternode weighs one in projections.
    pub mn_rr: bool,
}

impl MnList {
    /// `GetMNPayee(pindexPrev)` on this list: the masternode the next block pays. `None` for an
    /// empty list or when every masternode is banned (Core's `nullptr`; quirk 13). Unconfirmed
    /// masternodes are payable.
    // Core: evo/deterministicmns.cpp:158 CDeterministicMNList::GetMNPayee
    pub fn payee(&self, rules: PayeeRules) -> Option<&Arc<Dmn>> {
        if !self.fast_payee_ok() {
            return payee_scan(self, rules);
        }
        if rules.v19
            && !rules.mn_rr
            && let Some(dmn) = self.streak_candidate()
        {
            return Some(dmn);
        }
        // `CompareByLastPaid` is a strict total order: the linear minimum is the queue's first.
        self.payee_queue().next().and_then(|key| self.get(&key.pro_tx))
    }

    /// `GetProjectedMNPayees(pindexPrev, count)`: the next `count` payees, at most the weighted
    /// count of valid masternodes (the enabled count from MN_RR). Core's `nCount < 0 → {}` is the
    /// RPC's argument check.
    // Core: evo/deterministicmns.cpp:196 CDeterministicMNList::GetProjectedMNPayees
    pub fn projected_payees(&self, rules: PayeeRules, count: usize) -> Vec<Arc<Dmn>> {
        if !self.fast_payee_ok() {
            return scan::projected_scan(self, rules, count);
        }
        let count = projection_len(self, rules, count);
        let mut out: Vec<Arc<Dmn>> = Vec::with_capacity(count);
        let candidate = if rules.mn_rr {
            None
        } else {
            self.streak_candidate()
        };
        // P1: the candidate is the only continuing evonode, so its remaining payments come first
        // and the rest is the queue order with every masternode repeated by its weight; its
        // already-paid copies sit at its own queue position (equal keys are the same masternode,
        // so the sort's instability cannot show).
        if let Some(candidate) = candidate {
            push_copies(&mut out, candidate, remaining_payments(candidate), count);
        }
        for key in self.payee_queue() {
            if out.len() >= count {
                break;
            }
            let Some(dmn) = self.get(&key.pro_tx) else {
                continue;
            };
            let copies = match candidate {
                Some(c) if c.pro_tx_hash == dmn.pro_tx_hash => {
                    usize::try_from(dmn.hot.consecutive_payments).unwrap_or(0)
                }
                _ if rules.mn_rr => 1,
                _ => dmn.weight() as usize,
            };
            push_copies(&mut out, dmn, copies, count);
        }
        out
    }

    /// The continuing evonode (EV2 §3.4): a valid Evo paid at the list height with fewer than four
    /// consecutive payments. With P1 and P2 it is unique and its queue key is at least the list
    /// height, so the walk covers `pay_height >= height` (an unbounded range: a future revival
    /// height can put it above).
    fn streak_candidate(&self) -> Option<&Arc<Dmn>> {
        let height = self.height();
        self.payee_queue_from(height)
            .filter_map(|key| self.get(&key.pro_tx))
            .find(|dmn| continues_streak(dmn, height))
    }

    /// P1 ∧ P2, computed from the list on first use (EV2 §2.4, §3.4).
    pub(crate) fn fast_payee_ok(&self) -> bool {
        *self.fast_payee_ok.get_or_init(|| {
            let height = self.height();
            if height > 0 {
                // P2 holds. A valid masternode paid at a positive height has a queue key of at
                // least that height (its last payment is not 0, so the registration fallback
                // cannot fire, and a revival only raises the key).
                let paid = self
                    .payee_queue_from(height)
                    .filter(|key| {
                        self.get(&key.pro_tx).is_some_and(|d| d.hot.last_paid_height == height)
                    })
                    .take(2)
                    .count();
                paid <= 1
            } else {
                self.iter().all(|d| d.cold.registered_height >= 0)
                    && self
                        .iter_valid()
                        .filter(|d| d.hot.last_paid_height == height)
                        .take(2)
                        .count()
                        <= 1
            }
        })
    }
}

/// [`MnList::payee`]'s literal scan, kept out of line: inlined, it made the indexed path save
/// three more registers per call (+6 instructions, T30 PF0 dml-payee rows).
#[cold]
#[inline(never)]
fn payee_scan(list: &MnList, rules: PayeeRules) -> Option<&Arc<Dmn>> {
    scan::payee_scan(list, rules)
}

/// Appends `copies` copies of `dmn`, stopping at `count` entries.
fn push_copies(out: &mut Vec<Arc<Dmn>>, dmn: &Arc<Dmn>, copies: usize, count: usize) {
    let copies = copies.min(count.saturating_sub(out.len()));
    out.extend(std::iter::repeat_n(Arc::clone(dmn), copies));
}

#[cfg(test)]
mod tests;
