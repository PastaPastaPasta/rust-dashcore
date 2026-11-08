//! The secondary indexes and counts of a list (EV2 §2.4): pure functions of its masternodes,
//! maintained at the list's choke point, never serialized.

use imbl::OrdSet;

use crate::ProTxHash;
use crate::types::{Counts, Dmn, PayeeKey};

/// The four indexes EV2 §2.4 keeps, and `GetCounts()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Indexes {
    /// The valid masternodes in `CompareByLastPaid` order: Core's payee candidates.
    pub(crate) payee_queue: OrdSet<PayeeKey>,
    /// Valid masternodes with a PoSe penalty: `DecreaseScores`' candidates.
    pub(crate) decaying: OrdSet<ProTxHash>,
    /// Masternodes without a confirmed hash, by registration height: the confirmation step's
    /// candidates.
    pub(crate) unconfirmed: OrdSet<(i32, ProTxHash)>,
    /// Evo masternodes with a payment streak: the streak reset's candidates.
    pub(crate) evo_streak: OrdSet<ProTxHash>,
    /// `GetCounts()` in O(1).
    pub(crate) counts: Counts,
}

impl Indexes {
    /// Adds `dmn` to every index it belongs to.
    pub(crate) fn insert(&mut self, dmn: &Dmn) {
        self.step(dmn, true);
    }

    /// Removes `dmn`, which the indexes hold, from every index.
    pub(crate) fn remove(&mut self, dmn: &Dmn) {
        self.step(dmn, false);
    }

    fn step(&mut self, dmn: &Dmn, insert: bool) {
        fn toggle<T: Ord + Clone>(set: &mut OrdSet<T>, member: bool, key: T, insert: bool) {
            if member {
                if insert {
                    set.insert(key);
                } else {
                    set.remove(&key);
                }
            }
        }
        let pro_tx = dmn.pro_tx_hash;
        let valid = dmn.is_valid();
        toggle(&mut self.payee_queue, valid, dmn.payee_key(), insert);
        toggle(&mut self.decaying, valid && dmn.hot.pose_penalty > 0, pro_tx, insert);
        toggle(
            &mut self.unconfirmed,
            dmn.cold.confirmed_hash.is_null(),
            (dmn.cold.registered_height, pro_tx),
            insert,
        );
        toggle(
            &mut self.evo_streak,
            dmn.is_evo() && dmn.hot.consecutive_payments != 0,
            pro_tx,
            insert,
        );
        if insert {
            self.counts.add(dmn);
        } else {
            self.counts.remove(dmn);
        }
    }

    /// The indexes recomputed from scratch (invariant I-X).
    pub(crate) fn of<'a>(mns: impl Iterator<Item = &'a Dmn>) -> Indexes {
        let mut indexes = Indexes::default();
        for dmn in mns {
            indexes.insert(dmn);
        }
        indexes
    }
}
