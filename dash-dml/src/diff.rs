//! List diffs (EV2 §2.8) and the per-block DML records the store keeps (§2.9, §2.10).

use std::sync::Arc;

use dash_primitives::BlockHash;
use imbl::ordmap::DiffItem;

use crate::ProTxHash;
use crate::list::{MnList, MnListError};
use crate::types::{ColdDerived, Dmn, MnCold, MnHot, scheme_for};

/// Core's `CDeterministicMNListDiff`: what one block changed (`BuildDiff`, `deterministicmns.cpp`
/// v24:336-369).
///
/// The in-memory value and the canonical diff bytes are Core's. The store keeps it inside a
/// [`MnListDiffRecord`], which adds what Core's diff cannot reconstruct.
#[derive(Clone, Debug, Default)]
pub struct MnListDiff {
    /// Added masternodes, ascending `internal_id` (`BuildDiff` sorts them).
    pub added: Vec<Arc<Dmn>>,
    /// Updated states by internal id, ascending.
    pub updated: Vec<(u64, MnStateDiff)>,
    /// Removed internal ids, ascending (Core's `std::set`).
    pub removed: Vec<u64>,
}

impl MnListDiff {
    /// `HasChanges()`.
    pub fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.updated.is_empty() || !self.removed.is_empty()
    }
}

/// Core's `CDeterministicMNStateDiff`: a field mask (Core's `Field_*` bits, nVersion first,
/// `dmnstate.h` v24:180-204) and the new state the set fields are copied from.
///
/// `pubKeyOperator` is recorded as rc.4 records it (#7876, CE4-04): when the key's serialization
/// changes, or when the state's version moves between the legacy and the basic scheme.
#[derive(Clone, Debug)]
pub struct MnStateDiff {
    /// The changed fields.
    pub fields: u32,
    /// The state the set fields come from.
    pub(crate) hot: MnHot,
    pub(crate) cold: Arc<MnCold>,
}

impl MnStateDiff {
    /// A diff of `fields` whose values come from `hot` and `cold` (the codecs' constructor).
    pub(crate) fn from_parts(fields: u32, hot: MnHot, cold: Arc<MnCold>) -> MnStateDiff {
        MnStateDiff {
            fields,
            hot,
            cold,
        }
    }

    /// The state the set fields come from.
    pub(crate) fn parts(&self) -> (&MnHot, &MnCold) {
        (&self.hot, &self.cold)
    }

    /// `Field_nVersion`.
    pub const VERSION: u32 = 0x0001;

    /// `Field_nRegisteredHeight`.
    pub const REGISTERED_HEIGHT: u32 = 0x0002;
    /// `Field_nLastPaidHeight`.
    pub const LAST_PAID_HEIGHT: u32 = 0x0004;
    /// `Field_nPoSePenalty`.
    pub const POSE_PENALTY: u32 = 0x0008;
    /// `Field_nPoSeRevivedHeight`.
    pub const POSE_REVIVED_HEIGHT: u32 = 0x0010;
    /// `Field_nPoSeBanHeight`.
    pub const POSE_BAN_HEIGHT: u32 = 0x0020;
    /// `Field_nRevocationReason`.
    pub const REVOCATION_REASON: u32 = 0x0040;
    /// `Field_confirmedHash`.
    pub const CONFIRMED_HASH: u32 = 0x0080;
    /// `Field_confirmedHashWithProRegTxHash`.
    pub const CONFIRMED_HASH_WITH_PRO_TX: u32 = 0x0100;
    /// `Field_keyIDOwner`.
    pub const KEY_ID_OWNER: u32 = 0x0200;
    /// `Field_pubKeyOperator`.
    pub const PUB_KEY_OPERATOR: u32 = 0x0400;
    /// `Field_keyIDVoting`.
    pub const KEY_ID_VOTING: u32 = 0x0800;
    /// `Field_netInfo`.
    pub const NET_INFO: u32 = 0x1000;
    /// `Field_scriptPayout`.
    pub const SCRIPT_PAYOUT: u32 = 0x2000;
    /// `Field_scriptOperatorPayout`.
    pub const SCRIPT_OPERATOR_PAYOUT: u32 = 0x4000;
    /// `Field_nConsecutivePayments`.
    pub const CONSECUTIVE_PAYMENTS: u32 = 0x8000;
    /// `Field_platformNodeID`.
    pub const PLATFORM_NODE_ID: u32 = 0x1_0000;
    /// `Field_platformP2PPort`.
    pub const PLATFORM_P2P_PORT: u32 = 0x2_0000;
    /// `Field_platformHTTPPort`.
    pub const PLATFORM_HTTP_PORT: u32 = 0x4_0000;
    /// `Field_payouts` (V24).
    pub const PAYOUTS: u32 = 0x8_0000;
    /// `Field_shares` (V24).
    pub const SHARES: u32 = 0x10_0000;
    /// `Field_nEarlyPeriodBlocks` (V24).
    pub const EARLY_PERIOD_BLOCKS: u32 = 0x20_0000;
    /// `Field_nEarlyPenalty` (V24).
    pub const EARLY_PENALTY: u32 = 0x40_0000;
    /// Every bit Core defines.
    pub const ALL: u32 = 0x7f_ffff;
    /// The bits of [`MnHot`]'s fields.
    pub const HOT: u32 = Self::LAST_PAID_HEIGHT
        | Self::POSE_PENALTY
        | Self::POSE_REVIVED_HEIGHT
        | Self::POSE_BAN_HEIGHT
        | Self::CONSECUTIVE_PAYMENTS;

    /// `CDeterministicMNStateDiff(a, b)` (`dmnstate.h` v24:250-281): the fields in which `b`'s
    /// state differs from `a`'s, `netInfo` by deep comparison, and `nVersion` forced when
    /// `pubKeyOperator` or `netInfo` is set.
    ///
    /// `pubKeyOperator` is recorded when its captured-scheme serialization differs (`GetHash()`)
    /// or the legacy-ness of `nVersion` changes, which rc.4 adds so that a scheme migration with
    /// equal bytes is recorded too (CE4-04; it replaces the EV2 §3.7 erratum's union with v23's
    /// point rule). The comparison runs on clones: the states' keys are not touched.
    // Core (rc.4): evo/dmnstate.h:250-281, 259-270
    pub fn between(a: &Dmn, b: &Dmn) -> MnStateDiff {
        let mut fields = hot_fields(&a.hot, &b.hot);
        if !Arc::ptr_eq(&a.cold, &b.cold) {
            fields |= cold_fields(&a.cold, &b.cold);
        }
        if fields & (Self::PUB_KEY_OPERATOR | Self::NET_INFO) != 0 {
            fields |= Self::VERSION;
        }
        MnStateDiff {
            fields,
            hot: b.hot,
            cold: Arc::clone(&b.cold),
        }
    }

    /// `ApplyToState` (`dmnstate.h` v24:316-323): `hot` and `cold` with the set fields replaced.
    /// The cold part is rebuilt only if a cold field is set.
    // Core: evo/dmnstate.h:316 CDeterministicMNStateDiff::ApplyToState
    pub fn apply(&self, hot: &MnHot, cold: &Arc<MnCold>) -> (MnHot, Arc<MnCold>) {
        let f = |bit: u32| self.fields & bit != 0;
        let mut new_hot = *hot;
        let src = &self.hot;
        if f(Self::LAST_PAID_HEIGHT) {
            new_hot.last_paid_height = src.last_paid_height;
        }
        if f(Self::POSE_PENALTY) {
            new_hot.pose_penalty = src.pose_penalty;
        }
        if f(Self::POSE_REVIVED_HEIGHT) {
            new_hot.pose_revived_height = src.pose_revived_height;
        }
        if f(Self::POSE_BAN_HEIGHT) {
            new_hot.pose_ban_height = src.pose_ban_height;
        }
        if f(Self::CONSECUTIVE_PAYMENTS) {
            new_hot.consecutive_payments = src.consecutive_payments;
        }
        if self.fields & !Self::HOT == 0 {
            return (new_hot, Arc::clone(cold));
        }
        let mut new = (**cold).clone();
        let src = &*self.cold;
        macro_rules! copy {
            ($($bit:ident => $field:ident),* $(,)?) => {
                $(if f(Self::$bit) {
                    new.$field = src.$field.clone();
                })*
            };
        }
        copy!(
            VERSION => version,
            REGISTERED_HEIGHT => registered_height,
            REVOCATION_REASON => revocation_reason,
            CONFIRMED_HASH => confirmed_hash,
            CONFIRMED_HASH_WITH_PRO_TX => confirmed_hash_with_pro_tx,
            KEY_ID_OWNER => key_id_owner,
            PUB_KEY_OPERATOR => pub_key_operator,
            KEY_ID_VOTING => key_id_voting,
            NET_INFO => net_info,
            SCRIPT_PAYOUT => script_payout,
            PAYOUTS => payouts,
            SHARES => shares,
            EARLY_PERIOD_BLOCKS => early_period_blocks,
            EARLY_PENALTY => early_penalty,
            SCRIPT_OPERATOR_PAYOUT => script_operator_payout,
            PLATFORM_NODE_ID => platform_node_id,
            PLATFORM_P2P_PORT => platform_p2p_port,
            PLATFORM_HTTP_PORT => platform_http_port,
        );
        new.derived = ColdDerived::default();
        (new_hot, Arc::new(new))
    }
}

/// `mask` if `differs`, else no bits.
fn bit(differs: bool, mask: u32) -> u32 {
    if differs {
        mask
    } else {
        0
    }
}

fn hot_fields(a: &MnHot, b: &MnHot) -> u32 {
    bit(a.last_paid_height != b.last_paid_height, MnStateDiff::LAST_PAID_HEIGHT)
        | bit(a.pose_penalty != b.pose_penalty, MnStateDiff::POSE_PENALTY)
        | bit(a.pose_revived_height != b.pose_revived_height, MnStateDiff::POSE_REVIVED_HEIGHT)
        | bit(a.pose_ban_height != b.pose_ban_height, MnStateDiff::POSE_BAN_HEIGHT)
        | bit(a.consecutive_payments != b.consecutive_payments, MnStateDiff::CONSECUTIVE_PAYMENTS)
}

fn cold_fields(a: &MnCold, b: &MnCold) -> u32 {
    bit(a.version != b.version, MnStateDiff::VERSION)
        | bit(a.registered_height != b.registered_height, MnStateDiff::REGISTERED_HEIGHT)
        | bit(a.revocation_reason != b.revocation_reason, MnStateDiff::REVOCATION_REASON)
        | bit(a.confirmed_hash != b.confirmed_hash, MnStateDiff::CONFIRMED_HASH)
        | bit(
            a.confirmed_hash_with_pro_tx != b.confirmed_hash_with_pro_tx,
            MnStateDiff::CONFIRMED_HASH_WITH_PRO_TX,
        )
        | bit(a.key_id_owner != b.key_id_owner, MnStateDiff::KEY_ID_OWNER)
        | bit(operator_key_changed(a, b), MnStateDiff::PUB_KEY_OPERATOR)
        | bit(a.key_id_voting != b.key_id_voting, MnStateDiff::KEY_ID_VOTING)
        // `util::shared_ptr_not_equal`: a deep comparison.
        | bit(!Arc::ptr_eq(&a.net_info, &b.net_info) && a.net_info != b.net_info, MnStateDiff::NET_INFO)
        | bit(a.script_payout != b.script_payout, MnStateDiff::SCRIPT_PAYOUT)
        | bit(a.payouts != b.payouts, MnStateDiff::PAYOUTS)
        | bit(a.shares != b.shares, MnStateDiff::SHARES)
        | bit(a.early_period_blocks != b.early_period_blocks, MnStateDiff::EARLY_PERIOD_BLOCKS)
        | bit(a.early_penalty != b.early_penalty, MnStateDiff::EARLY_PENALTY)
        | bit(a.script_operator_payout != b.script_operator_payout, MnStateDiff::SCRIPT_OPERATOR_PAYOUT)
        | bit(a.platform_node_id != b.platform_node_id, MnStateDiff::PLATFORM_NODE_ID)
        | bit(a.platform_p2p_port != b.platform_p2p_port, MnStateDiff::PLATFORM_P2P_PORT)
        | bit(a.platform_http_port != b.platform_http_port, MnStateDiff::PLATFORM_HTTP_PORT)
}

/// rc.4's rule for `PUB_KEY_OPERATOR`: the captured-scheme serialization (`GetHash()`) differs,
/// or one state's version is the legacy scheme's and the other's is not. On clones only.
// Core (rc.4): evo/dmnstate.h:259-270
fn operator_key_changed(a: &MnCold, b: &MnCold) -> bool {
    a.pub_key_operator.clone().to_bytes() != b.pub_key_operator.clone().to_bytes()
        || scheme_for(a.version) != scheme_for(b.version)
}

impl MnList {
    /// `BuildDiff(to)` (`deterministicmns.cpp` v24:336-369): what changed from `self` to `to`.
    ///
    /// It walks `imbl`'s diff, which skips subtrees the two lists share, so the cost is
    /// O(changes · log n). An entry `to` replaced becomes [`MnStateDiff::between`], kept if any
    /// field differs; added entries are sorted by internal id, the updated and removed ids
    /// ascend.
    // Core: evo/deterministicmns.cpp:336 CDeterministicMNList::BuildDiff
    pub fn diff_to(&self, to: &MnList) -> MnListDiff {
        let mut diff = MnListDiff::default();
        for item in self.mns.diff(&to.mns) {
            match item {
                DiffItem::Add(_, new) => diff.added.push(Arc::clone(&new.0)),
                DiffItem::Update {
                    old: (_, old),
                    new: (_, new),
                } => {
                    let state = MnStateDiff::between(&old.0, &new.0);
                    if state.fields != 0 {
                        diff.updated.push((new.0.internal_id, state));
                    }
                }
                DiffItem::Remove(_, old) => diff.removed.push(old.0.internal_id),
            }
        }
        diff.added.sort_by_key(|dmn| dmn.internal_id);
        diff.updated.sort_by_key(|(id, _)| *id);
        diff.removed.sort_unstable();
        diff
    }

    /// `ApplyDiff(pindex, diff)` (`deterministicmns.cpp` v24:371-393): the position, then the
    /// removals, the additions (each `AddMN(dmn, true)`) and the updates, ascending. All or
    /// nothing. The list is then [`Provenance::Decoded`](crate::Provenance::Decoded).
    ///
    /// Not Core's: an addition or update that leaves a state outside Core's state domain is
    /// refused ([`MnListError::VersionHiddenField`], EV2a D20). Core's diff carries any masked
    /// field whatever the version, and its `ApplyDiff` applies, for example, a `scriptPayout` to
    /// a version-3 state. No diff between two lists Core writes does that, nor any record of
    /// ours; only a crafted or foreign diff can.
    // Core: evo/deterministicmns.cpp:371 CDeterministicMNList::ApplyDiff
    pub fn apply_diff(
        &mut self,
        block: BlockHash,
        height: i32,
        diff: &MnListDiff,
    ) -> Result<(), MnListError> {
        let mut next = self.clone();
        next.set_position(block, height);
        for id in &diff.removed {
            let pro_tx =
                next.get_by_internal_id(*id).ok_or(MnListError::RemovedNotFound(*id))?.pro_tx_hash;
            next.remove(&pro_tx)?;
        }
        for dmn in &diff.added {
            next.try_add(Arc::clone(dmn), true)?;
        }
        for (id, state) in &diff.updated {
            let old = next.get_by_internal_id(*id).ok_or(MnListError::UpdatedNotFound(*id))?;
            let pro_tx = old.pro_tx_hash;
            let (hot, cold) = state.apply(&old.hot, &old.cold);
            next.update(&pro_tx, hot, cold)?;
        }
        next.mark_decoded();
        *self = next;
        Ok(())
    }

    /// [`MnList::apply_diff`] with a stored record, then the record's counter and collision
    /// buckets (EV2 §2.8, §2.9): the list it gives equals the online list, `core_order()`
    /// included.
    pub fn apply_record(
        &mut self,
        block: BlockHash,
        record: &MnListDiffRecord,
    ) -> Result<(), MnListError> {
        let mut next = self.clone();
        next.apply_diff(block, record.height, &record.diff)?;
        next.set_total_registered(record.total_registered_after);
        next.restore_buckets(
            record.collision_buckets_after.iter().map(|(h, seq)| (*h, seq.as_slice())),
        )?;
        *self = next;
        Ok(())
    }
}

impl MnListDiffRecord {
    /// The record of the block that turned `prev` into `next`: Core's diff, `next`'s counter, and
    /// the collision trailer, which holds `next`'s sequence (empty if none) for every hash whose
    /// bucket changed and every hash a diff entry touches that has a bucket in either list, so
    /// [`MnList::apply_record`] restores `next`'s order whatever order the diff's sets leave.
    pub fn build(prev: &MnList, next: &MnList) -> MnListDiffRecord {
        let diff = prev.diff_to(next);
        let mut hashes: Vec<u64> = Vec::new();
        for item in prev.collision_buckets_map().diff(next.collision_buckets_map()) {
            hashes.push(match item {
                DiffItem::Add(h, _) | DiffItem::Remove(h, _) => *h,
                DiffItem::Update {
                    new: (h, _),
                    ..
                } => *h,
            });
        }
        let touched = diff
            .added
            .iter()
            .map(|d| d.pro_tx_hash)
            .chain(
                diff.updated
                    .iter()
                    .filter_map(|(id, _)| next.get_by_internal_id(*id).map(|d| d.pro_tx_hash)),
            )
            .chain(
                diff.removed
                    .iter()
                    .filter_map(|id| prev.get_by_internal_id(*id).map(|d| d.pro_tx_hash)),
            )
            .map(|p| p.immer_hash())
            .filter(|h| prev.collision_bucket(*h).is_some() || next.collision_bucket(*h).is_some());
        hashes.extend(touched);
        hashes.sort_unstable();
        hashes.dedup();
        let collision_buckets_after = hashes
            .into_iter()
            .map(|h| {
                (
                    h,
                    next.collision_bucket(h)
                        .map(|b| b.iter().copied().collect())
                        .unwrap_or_default(),
                )
            })
            .collect();
        MnListDiffRecord {
            height: next.height(),
            total_registered_after: next.total_registered(),
            diff,
            collision_buckets_after,
        }
    }
}

/// What the store keeps for one block (EV2 §2.8, §2.9 trailer): Core's diff plus what Core's
/// diff cannot reconstruct, so that a reconstructed list equals the online list byte for byte
/// (`total_registered`, internal ids, collision-bucket order).
#[derive(Clone, Debug)]
pub struct MnListDiffRecord {
    /// The block's height.
    pub height: i32,
    /// `nTotalRegisteredCount` after the block. Core's diff cannot restore it after a
    /// register-and-spend in one block (EV2 rev1, GPT F2).
    pub total_registered_after: u32,
    /// Core's diff.
    pub diff: MnListDiff,
    /// Every collision bucket whose sequence the block changed, including order-only changes,
    /// front first.
    pub collision_buckets_after: Vec<(u64, Vec<ProTxHash>)>,
}

/// The DML part of a block's transactional delta (EV2 §2.10): goes into [`crate::store::EvoDelta`]
/// and so into the Durable B1 batch (A1 §3.5.4).
#[derive(Clone, Debug)]
pub struct DmlDelta {
    /// The block.
    pub block: BlockHash,
    /// Its height.
    pub height: i32,
    /// The `mn_diff` row, written for every DIP3-active block, empty diffs included.
    pub record: Arc<MnListDiffRecord>,
    /// The `mn_snapshot` row: every 576 blocks and at the first DIP3 block (EV2 §2.10).
    pub snapshot: Option<Arc<MnList>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MnType;
    use crate::record::SameRecord;
    use crate::testkit::{dmn, dmn_with, operator_key};
    use dash_pkc::bls::BlsSchemeKind;

    fn with_cold(base: &Arc<Dmn>, f: impl FnOnce(&mut MnCold)) -> Dmn {
        let mut cold = (*base.cold).clone();
        f(&mut cold);
        cold.derived = ColdDerived::default();
        Dmn {
            cold: Arc::new(cold),
            ..(**base).clone()
        }
    }

    /// `apply(between(a, b))` on `a` gives `b`; a hot-only change keeps `a`'s cold part.
    #[test]
    fn apply_inverts_between() {
        let a = dmn_with(1, MnType::Evo, |_, c| {
            c.pub_key_operator = operator_key(1, BlsSchemeKind::Legacy)
        });
        let b = with_cold(&a, |c| {
            c.key_id_voting = [7; 20].into();
            c.platform_http_port = 443;
        });
        let d = MnStateDiff::between(&a, &b);
        assert_eq!(d.fields, MnStateDiff::KEY_ID_VOTING | MnStateDiff::PLATFORM_HTTP_PORT);
        let (hot, cold) = d.apply(&a.hot, &a.cold);
        assert_eq!(hot, b.hot);
        assert!(cold.same_record(&b.cold));

        let mut hot_only = (*a).clone();
        hot_only.hot.pose_penalty = 66;
        let d = MnStateDiff::between(&a, &hot_only);
        assert_eq!(d.fields, MnStateDiff::POSE_PENALTY);
        let (hot, cold) = d.apply(&a.hot, &a.cold);
        assert_eq!(hot.pose_penalty, 66);
        assert!(Arc::ptr_eq(&cold, &a.cold));
        assert_eq!(MnStateDiff::between(&a, &a).fields, 0);
    }

    /// `pubKeyOperator` and `netInfo` force `nVersion`.
    #[test]
    fn key_and_net_info_force_the_version() {
        let a = dmn(1, MnType::Regular);
        let b = with_cold(&a, |c| c.pub_key_operator = operator_key(3, BlsSchemeKind::Legacy));
        let d = MnStateDiff::between(&a, &b);
        assert_eq!(d.fields, MnStateDiff::PUB_KEY_OPERATOR | MnStateDiff::VERSION);
    }

    /// The erratum's union rule: one point re-encoded under a new version is recorded; the same
    /// key re-read from its bytes is not.
    #[test]
    fn operator_keys_are_recorded_by_point_or_encoding() {
        let a = dmn_with(1, MnType::Regular, |_, c| {
            c.pub_key_operator = operator_key(4, BlsSchemeKind::Legacy)
        });
        let re_encoded = with_cold(&a, |c| {
            c.version = 2;
            c.pub_key_operator = operator_key(4, BlsSchemeKind::Basic);
        });
        let d = MnStateDiff::between(&a, &re_encoded);
        assert_eq!(d.fields, MnStateDiff::PUB_KEY_OPERATOR | MnStateDiff::VERSION);
        let raw = with_cold(&a, |c| {
            c.pub_key_operator = crate::OperatorKey::from_bytes(
                &operator_key(4, BlsSchemeKind::Legacy).to_bytes(),
                dash_pkc::bls::BlsSchemeSource::Version(BlsSchemeKind::Legacy),
            );
        });
        assert_eq!(MnStateDiff::between(&a, &raw).fields, 0);
    }

    /// rc.4 records no point-only change (it dropped with v23's rule): the default key and a key
    /// set to a failed decode's object serialize as zeros under either scheme, so `GetHash()`
    /// matches and the version does not move, though Core's `operator==` tells them apart (the
    /// failed-decode case of `record::tests::a_key_set_to_a_failed_decode_is_not_the_default_key`).
    #[test]
    fn a_point_only_key_change_is_not_recorded() {
        let a = dmn_with(1, MnType::Regular, |_, c| {
            c.version = 2;
            c.pub_key_operator = crate::OperatorKey::new(BlsSchemeKind::Basic);
        });
        let failed = crate::OperatorKey::from_bytes(
            &[0xff; 48],
            dash_pkc::bls::BlsSchemeSource::Version(BlsSchemeKind::Basic),
        )
        .get();
        assert!(!failed.is_valid());
        let b = Arc::new(with_cold(&a, |c| {
            c.pub_key_operator = crate::OperatorKey::from_value(failed, BlsSchemeKind::Basic)
        }));
        for scheme in [BlsSchemeKind::Legacy, BlsSchemeKind::Basic] {
            assert_eq!(a.cold.pub_key_operator.clone().to_bytes_as(scheme), [0u8; 48]);
            assert_eq!(b.cold.pub_key_operator.clone().to_bytes_as(scheme), [0u8; 48]);
        }
        assert!(a.cold.pub_key_operator.clone() != b.cold.pub_key_operator.clone());
        assert_eq!(MnStateDiff::between(&a, &b).fields, 0);
        assert_eq!(MnStateDiff::between(&b, &a).fields, 0);
    }

    /// CE4-04 (#7876): a scheme migration whose key bytes stay is recorded, in both directions,
    /// whether the bytes are a key under both schemes or under neither (`dmnstate.h`
    /// rc.4:259-270); rc.3 compared `GetHash()` alone. A version change within one scheme is not.
    #[test]
    fn a_scheme_change_with_equal_bytes_is_recorded() {
        let valid = operator_key(4, BlsSchemeKind::Legacy).to_bytes();
        for bytes in [valid, [0xff; 48]] {
            let state = |version, scheme| {
                dmn_with(1, MnType::Regular, |_, c| {
                    c.version = version;
                    c.pub_key_operator = crate::OperatorKey::from_bytes(
                        &bytes,
                        dash_pkc::bls::BlsSchemeSource::Version(scheme),
                    );
                })
            };
            let (legacy, basic) = (state(1, BlsSchemeKind::Legacy), state(2, BlsSchemeKind::Basic));
            assert_eq!(
                legacy.cold.pub_key_operator.clone().to_bytes(),
                basic.cold.pub_key_operator.clone().to_bytes()
            );
            let both = MnStateDiff::PUB_KEY_OPERATOR | MnStateDiff::VERSION;
            assert_eq!(MnStateDiff::between(&legacy, &basic).fields, both, "{bytes:02x?}");
            assert_eq!(MnStateDiff::between(&basic, &legacy).fields, both, "{bytes:02x?}");
            let ext = state(3, BlsSchemeKind::Basic);
            assert_eq!(MnStateDiff::between(&basic, &ext).fields, MnStateDiff::VERSION);
        }
    }

    /// Records compare by the mask and the set fields only.
    #[test]
    fn state_diffs_compare_by_their_set_fields() {
        let a = dmn(1, MnType::Regular);
        let b = with_cold(&a, |c| c.key_id_voting = [7; 20].into());
        let online = MnStateDiff::between(&a, &b);
        // The same fields over another base state.
        let other_base = with_cold(&dmn(2, MnType::Regular), |c| c.key_id_voting = [7; 20].into());
        let decoded = MnStateDiff {
            fields: online.fields,
            hot: MnHot::default(),
            cold: Arc::clone(&other_base.cold),
        };
        assert!(online.same_record(&decoded));
        let changed = with_cold(&a, |c| c.key_id_voting = [8; 20].into());
        assert!(!online.same_record(&MnStateDiff::between(&a, &changed)));
    }
}
