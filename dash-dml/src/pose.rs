//! Proof of service (EV2 §3.3, quirk 24) and the application order of the bulk phases (§2.7).

use crate::ProTxHash;
use crate::list::{MnList, MnListError};
use crate::trace::{DmlEvent, DmlTrace};
use crate::types::MnHot;

impl MnList {
    /// `CalcMaxPoSePenalty()`: the number of masternodes, banned included, but at least 100;
    /// evaluated on the list as it stands (inside a build, the list rebuilt so far).
    // Core: evo/deterministicmns.cpp:267 CDeterministicMNList::CalcMaxPoSePenalty
    pub fn calc_max_pose_penalty(&self) -> i32 {
        // `static_cast<int>(size_t)`: the low 32 bits.
        100.max(self.counts().total() as i32)
    }

    /// `CalcPenalty(percent)`: `max * percent / 100` in Core's `int` (wrapping where Core
    /// overflows; EV2 §3.11 item 2).
    // Core: evo/deterministicmns.cpp:275 CDeterministicMNList::CalcPenalty
    pub fn calc_penalty(&self, percent: i32) -> i32 {
        self.calc_max_pose_penalty().wrapping_mul(percent) / 100
    }

    /// `PoSePunish(proTxHash, penalty)`: the penalty grows (capped at the maximum) **even when
    /// the masternode is already banned**; one that is not banned is banned at the list height
    /// when it reaches the maximum.
    // Core: evo/deterministicmns.cpp:281 CDeterministicMNList::PoSePunish
    pub(crate) fn pose_punish(
        &mut self,
        pro_tx: &ProTxHash,
        penalty: i32,
        trace: &mut dyn DmlTrace,
    ) -> Result<(), MnListError> {
        let old = self.get(pro_tx).ok_or(MnListError::NotFound("PoSePunish", *pro_tx))?.hot;
        let max = self.calc_max_pose_penalty();
        let mut hot = old;
        hot.pose_penalty = hot.pose_penalty.wrapping_add(penalty).min(max);
        if !old.is_banned() {
            if hot.pose_penalty >= max {
                hot.ban_if_not_banned(self.height());
            }
            trace.event(DmlEvent::Punished {
                pro_tx: *pro_tx,
                height: self.height(),
                banned: hot.is_banned(),
                from: old.pose_penalty,
                to: hot.pose_penalty,
                max,
            });
        }
        self.update_hot_mn(pro_tx, hot)
    }

    /// `DecreaseScores()`: every valid masternode with a penalty loses one point, applied in
    /// the phase-start list's Core order ([`phase_order`]).
    // Core: evo/deterministicmns.cpp:309 CDeterministicMNList::DecreaseScores, :327 PoSeDecrease
    pub(crate) fn decrease_scores(&mut self) -> Result<(), MnListError> {
        let candidates: Vec<ProTxHash> = self.decaying().copied().collect();
        for pro_tx in phase_order(self, candidates) {
            self.update_hot_with(&pro_tx, |hot| hot.pose_penalty -= 1)?;
        }
        Ok(())
    }

    /// `UpdateMN` of the hot fields changed by `f`.
    pub(crate) fn update_hot_with(
        &mut self,
        pro_tx: &ProTxHash,
        f: impl FnOnce(&mut MnHot),
    ) -> Result<(), MnListError> {
        let mut hot = self.get(pro_tx).ok_or(MnListError::NotFound("UpdateMN", *pro_tx))?.hot;
        f(&mut hot);
        self.update_hot_mn(pro_tx, hot)
    }
}

/// The order in which Core applies a bulk phase's updates to `candidates` (EV2 §2.7, quirk 41):
/// Core takes a snapshot in CHAMP order when the phase starts (`start`) and `set`s each eligible
/// masternode in that order. Only the order inside a collision bucket is observable (updates to
/// different 64-bit hashes commute), so candidates sharing a bucket of `start` are put in the
/// bucket's sequence and every other candidate keeps its index order. With no buckets, as on
/// every real network, `candidates` is returned unchanged.
pub(crate) fn phase_order(start: &MnList, mut candidates: Vec<ProTxHash>) -> Vec<ProTxHash> {
    if start.collision_buckets_map().is_empty() {
        return candidates;
    }
    // A stable sort: candidates outside every bucket keep their order and come first.
    candidates.sort_by_key(|pro_tx| {
        let hash = pro_tx.immer_hash();
        match start.collision_bucket(hash) {
            None => (false, 0, 0),
            Some(bucket) => {
                let at = bucket.iter().position(|k| k == pro_tx).unwrap_or(usize::MAX);
                (true, hash, at)
            }
        }
    });
    candidates
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::MnType;
    use crate::testkit::{self, dmn};

    fn list_of(n: u8) -> MnList {
        let mut list = MnList::default_pre_dip3().with_height(10);
        for i in 1..=n {
            list.try_add(dmn(i, MnType::Regular), true).unwrap();
        }
        list
    }

    #[test]
    fn max_penalty_is_at_least_100_and_counts_banned() {
        let mut list = list_of(3);
        assert_eq!(list.calc_max_pose_penalty(), 100);
        assert_eq!(list.calc_penalty(66), 66);
        for i in 4..=120u8 {
            let mut d = (*dmn(i, MnType::Regular)).clone();
            d.hot.ban_if_not_banned(5);
            list.try_add(Arc::new(d), true).unwrap();
        }
        assert_eq!(list.calc_max_pose_penalty(), 120);
        assert_eq!(list.calc_penalty(66), 79);
    }

    /// Quirk 24 (Core's vectors: `tests/quirks_ev2b.rs`): the penalty grows on a banned
    /// masternode, capped at the maximum, and the ban height does not move.
    #[test]
    fn banned_masternodes_keep_accumulating() {
        let mut list = list_of(2);
        let a = testkit::pro_tx(1);
        list.pose_punish(&a, 66, &mut crate::NoTrace).unwrap();
        assert_eq!((list.get(&a).unwrap().hot.pose_penalty, list.is_pose_banned(&a)), (66, false));
        let mut log = Vec::new();
        list.pose_punish(&a, 66, &mut log).unwrap();
        assert_eq!(list.get(&a).unwrap().hot.pose_penalty, 100);
        assert_eq!(list.get(&a).unwrap().hot.banned_height(), 10);
        assert!(matches!(
            log[0],
            DmlEvent::Punished {
                banned: true,
                from: 66,
                to: 100,
                ..
            }
        ));
        let mut later = list.clone().with_height(11);
        later.pose_punish(&a, 1, &mut log).unwrap();
        // Banned already: no log line, the ban height stays, the penalty stays capped.
        assert_eq!(log.len(), 1);
        assert_eq!(later.get(&a).unwrap().hot.banned_height(), 10);
        assert_eq!(later.get(&a).unwrap().hot.pose_penalty, 100);
    }

    #[test]
    fn decay_touches_valid_masternodes_only() {
        let mut list = list_of(3);
        let (a, b) = (testkit::pro_tx(1), testkit::pro_tx(2));
        list.update_hot_with(&a, |h| h.pose_penalty = 3).unwrap();
        list.update_hot_with(&b, |h| {
            h.pose_penalty = 3;
            h.ban_if_not_banned(9);
        })
        .unwrap();
        list.decrease_scores().unwrap();
        assert_eq!(list.get(&a).unwrap().hot.pose_penalty, 2);
        assert_eq!(list.get(&b).unwrap().hot.pose_penalty, 3);
        list.check_invariants().unwrap();
    }
}
