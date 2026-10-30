//! Record equality that never changes the compared values (EV0 API review r1, M1).
//!
//! The DML values hold lazily decoded operator keys, whose `operator==` may decode both sides,
//! after which an undecodable key serializes as zeros (`BlsLazy`, BS11a-t). So none of these
//! types implement `PartialEq`: [`SameRecord`] compares what a record *is* (its stored form), for
//! tests, the in-memory store's rewrite check and xcheck comparators, and
//! [`crate::MnCold::core_eq`] is Core's comparison for the places Core compares.
//!
//! A record is compared as Core serializes it **and** as Core compares its operator key: the key
//! must serialize the same under the state's version scheme (what the state's
//! `CBLSLazyPublicKeyVersionWrapper` writes, legacy iff `nVersion == 1`) and be equal under
//! Core's `CBLSLazyWrapper::operator==`. Both are evaluated on clones, so the compared keys are
//! not touched at all (EV0 API review r2-m1, r2-m2). Serialization alone does not identify an
//! in-memory key, so both conditions are required. For records decoded from bytes the two conditions coincide, so this is the comparison Core's
//! `WriteDerived` makes (`evo/evodb.h` v24:159-205) and the one EV2a's canonical codec gives;
//! EV2 §2.8's online-vs-reconstructed equality is written with it.

use std::sync::Arc;

use crate::creditpool::{CreditBlockData, PoolState};
use crate::diff::{DmlDelta, MnListDiff, MnListDiffRecord, MnStateDiff};
use crate::mnhf::EhfSignals;
use crate::types::{Dmn, MnCold};

/// Equality of the stored form, with no change to the compared values or to any later
/// serialization of them.
pub trait SameRecord {
    /// Whether `self` and `other` are the same record.
    fn same_record(&self, other: &Self) -> bool;
}

/// Shared values short-circuit on pointer equality.
impl<T: SameRecord + ?Sized> SameRecord for Arc<T> {
    fn same_record(&self, other: &Self) -> bool {
        Arc::ptr_eq(self, other) || (**self).same_record(other)
    }
}

impl<T: SameRecord> SameRecord for [T] {
    fn same_record(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().zip(other).all(|(a, b)| a.same_record(b))
    }
}

impl<T: SameRecord> SameRecord for Option<T> {
    fn same_record(&self, other: &Self) -> bool {
        match (self, other) {
            (Some(a), Some(b)) => a.same_record(b),
            (None, None) => true,
            _ => false,
        }
    }
}

/// Whether two states hold the same operator key: the same serialization under each state's
/// version scheme (`dmnstate.h` v24:107), and equal under Core's `CBLSLazyWrapper::operator==`
/// (`bls/bls.h` v24:514).
///
/// Both run on **clones**: `BlsLazy::clone` copies the state, so neither the serialization nor a
/// decode `operator==` makes reaches the compared keys, which keep their buffer, object and
/// scheme exactly as they were; no later serialization or comparison of them (`core_eq`
/// included) can differ (r2-m1). The captured scheme is not compared on its own: two default
/// keys are the same whatever scheme they captured, which matters for a revoked masternode,
/// whose `ResetOperatorFields` key captures the global flag while a reconstructed one captures
/// the version's (r2-m2).
fn same_operator_key(a: &MnCold, b: &MnCold) -> bool {
    a.operator_key_bytes() == b.operator_key_bytes()
        && a.pub_key_operator.clone() == b.pub_key_operator.clone()
}

impl SameRecord for MnCold {
    /// The fields Core serializes for the state's version, in its member order
    /// (`dmnstate.h` v24:93-125), with the operator key as `same_operator_key` compares it (see the
    /// module docs). A version-3 (ExtAddr) state stores the payouts, the shares and the
    /// early-period fields but not `scriptPayout` or the platform ports; an older state stores the
    /// reverse. In memory both sets exist, and Core's in-memory diff compares all of them, so two
    /// states can differ only in fields their version does not store (O2a-EV). They are the same
    /// record.
    fn same_record(&self, other: &Self) -> bool {
        let ext_addr = self.version >= 3; // ProTxVersion::ExtAddr
        let by_version = if ext_addr {
            self.payouts == other.payouts
                && self.shares == other.shares
                && self.early_period_blocks == other.early_period_blocks
                && self.early_penalty == other.early_penalty
        } else {
            self.script_payout == other.script_payout
                && self.platform_p2p_port == other.platform_p2p_port
                && self.platform_http_port == other.platform_http_port
        };
        self.version == other.version
            && self.registered_height == other.registered_height
            && self.revocation_reason == other.revocation_reason
            && self.confirmed_hash == other.confirmed_hash
            && self.confirmed_hash_with_pro_tx == other.confirmed_hash_with_pro_tx
            && self.key_id_owner == other.key_id_owner
            && same_operator_key(self, other)
            && self.key_id_voting == other.key_id_voting
            && self.net_info == other.net_info
            && by_version
            && self.script_operator_payout == other.script_operator_payout
            && self.platform_node_id == other.platform_node_id
    }
}

impl SameRecord for Dmn {
    fn same_record(&self, other: &Self) -> bool {
        self.pro_tx_hash == other.pro_tx_hash
            && self.internal_id == other.internal_id
            && self.collateral == other.collateral
            && self.operator_reward == other.operator_reward
            && self.mn_type == other.mn_type
            && self.hot == other.hot
            && self.cold.same_record(&other.cold)
    }
}

impl SameRecord for MnStateDiff {
    /// The mask and the set fields as Core serializes them: the other fields are not part of
    /// the record (a decoded diff holds defaults there, an online one the new state's values).
    /// Unlike a state, a diff writes every set field whatever the version, so a version-3 diff
    /// can carry `scriptPayout` (O2a-EV). A set operator key must also be the same key
    /// (`same_operator_key`).
    fn same_record(&self, other: &Self) -> bool {
        let bytes = |diff: &MnStateDiff| {
            let mut out = Vec::new();
            crate::codec::write_state_diff(diff, &mut out);
            out
        };
        self.fields == other.fields
            && bytes(self) == bytes(other)
            && (self.fields & MnStateDiff::PUB_KEY_OPERATOR == 0
                || same_operator_key(self.parts().1, other.parts().1))
    }
}

impl SameRecord for MnListDiff {
    fn same_record(&self, other: &Self) -> bool {
        self.added.same_record(&other.added)
            && self.updated.len() == other.updated.len()
            && self
                .updated
                .iter()
                .zip(&other.updated)
                .all(|((a_id, a), (b_id, b))| a_id == b_id && a.same_record(b))
            && self.removed == other.removed
    }
}

impl SameRecord for MnListDiffRecord {
    fn same_record(&self, other: &Self) -> bool {
        self.height == other.height
            && self.total_registered_after == other.total_registered_after
            && self.diff.same_record(&other.diff)
            && self.collision_buckets_after == other.collision_buckets_after
    }
}

impl SameRecord for DmlDelta {
    fn same_record(&self, other: &Self) -> bool {
        self.block == other.block
            && self.height == other.height
            && self.record.same_record(&other.record)
            && self.snapshot.same_record(&other.snapshot)
    }
}

/// No lazy fields: plain equality is the stored form (an exception compares by kind and value,
/// as Core rethrows the same one at every read).
impl SameRecord for PoolState {
    fn same_record(&self, other: &Self) -> bool {
        self == other
    }
}

/// No lazy fields: plain equality is the stored form.
impl SameRecord for CreditBlockData {
    fn same_record(&self, other: &Self) -> bool {
        self == other
    }
}

/// No lazy fields: plain equality is the stored form.
impl SameRecord for EhfSignals {
    fn same_record(&self, other: &Self) -> bool {
        self == other
    }
}

#[cfg(test)]
mod tests {
    use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind, BlsSchemeSource};

    use super::*;
    use crate::MnType;
    use crate::testkit::cold_state;

    /// A G1 point canonical under both schemes (base-sdk's corpus `sub_100` key).
    const VALID: [u8; 48] = [
        0x88, 0x23, 0x54, 0xca, 0x87, 0xdf, 0x4b, 0xd9, 0x3d, 0x56, 0xce, 0xc1, 0x59, 0x93, 0xee,
        0xe0, 0x30, 0xcf, 0x14, 0x7a, 0x98, 0x49, 0x4e, 0x45, 0x2b, 0x95, 0x8d, 0xff, 0x9c, 0xb8,
        0xc5, 0x2e, 0x05, 0x0b, 0xc2, 0x02, 0xf5, 0xbc, 0x78, 0x1c, 0xd0, 0x62, 0x16, 0xc3, 0xc3,
        0xa5, 0x9b, 0xf8,
    ];
    /// Not a canonical encoding under the basic scheme: the decode fails.
    const UNDECODABLE: [u8; 48] = [0xff; 48];

    /// A version-2 state: its operator key is stored under the basic scheme.
    fn basic_state() -> MnCold {
        MnCold {
            version: 2,
            ..cold_state(1, MnType::Regular)
        }
    }

    fn basic(bytes: &[u8; 48]) -> BlsLazyPk {
        BlsLazyPk::from_bytes(bytes, BlsSchemeSource::Version(BlsSchemeKind::Basic))
    }

    /// A state holding raw `UNDECODABLE` bytes, and one holding the decoded `VALID` key: the pair
    /// on which Core's `operator==` decodes both sides.
    fn pair() -> (MnCold, MnCold) {
        let mut raw = basic_state();
        raw.pub_key_operator = basic(&UNDECODABLE);
        let mut decoded = basic_state();
        decoded.pub_key_operator = BlsLazyPk::from_value(basic(&VALID).get(), BlsSchemeKind::Basic);
        (raw, decoded)
    }

    #[test]
    fn same_record_never_changes_a_later_serialization() {
        let (raw, decoded) = pair();
        assert!(!raw.same_record(&decoded));
        assert!(!decoded.same_record(&raw));
        // Nothing was decoded: the raw key still serializes as read.
        assert_eq!(raw.pub_key_operator.to_bytes(), UNDECODABLE);
        assert_eq!(decoded.pub_key_operator.to_bytes(), VALID);
        // A key whose decode already failed serializes as zeros, and comparing keeps it so.
        let mut failed = basic_state();
        failed.pub_key_operator = basic(&UNDECODABLE);
        assert!(!failed.pub_key_operator.get().is_valid());
        assert_eq!(failed.pub_key_operator.to_bytes(), [0u8; 48]);
        assert!(!failed.same_record(&raw));
        assert_eq!(failed.pub_key_operator.to_bytes(), [0u8; 48]);
        assert_eq!(raw.pub_key_operator.to_bytes(), UNDECODABLE);
        // Equal records are equal, also across clones.
        assert!(raw.same_record(&raw.clone()));
        assert!(decoded.same_record(&decoded.clone()));
    }

    /// r2-m1 (the reviewer's probe): a comparison leaves a later `core_eq` on the same states
    /// exactly as Core's, which decodes the raw key and leaves it serializing as zeros.
    #[test]
    fn same_record_leaves_a_later_core_eq_as_core() {
        let (raw, decoded) = pair();
        let (without_a, without_b) = (decoded.clone(), raw.clone());
        assert!(!without_a.core_eq(&without_b));
        assert_eq!(without_b.pub_key_operator.to_bytes(), [0u8; 48]);

        let (with_a, with_b) = (decoded.clone(), raw.clone());
        assert!(with_a.same_record(&with_a.clone()));
        assert!(!with_a.same_record(&with_b));
        assert!(!with_a.core_eq(&with_b));
        assert_eq!(with_b.pub_key_operator.to_bytes(), without_b.pub_key_operator.to_bytes());
    }

    /// r2-m2 (the reviewer's probe): default keys serialize as zeros whatever scheme they
    /// captured, so records holding them are the same, as Core's serialization and `operator==`
    /// say. The revoked masternode: `ResetOperatorFields` builds the key under the global flag
    /// (basic after V19) with `nVersion = 1`, a reconstruction builds it under the version's
    /// (legacy) scheme.
    #[test]
    fn default_keys_compare_as_core_serializes() {
        let mut online = cold_state(1, MnType::Regular);
        online.pub_key_operator = BlsLazyPk::new(BlsSchemeKind::Basic);
        let mut reconstructed = cold_state(1, MnType::Regular);
        reconstructed.pub_key_operator =
            BlsLazyPk::from_bytes(&[0u8; 48], BlsSchemeSource::Version(BlsSchemeKind::Legacy));
        assert!(online.core_eq(&reconstructed));
        assert!(online.same_record(&reconstructed));
        assert!(reconstructed.same_record(&online));
        // Neither key changed: both still captured their own scheme and serialize as zeros.
        assert_eq!(online.pub_key_operator.scheme(), BlsSchemeKind::Basic);
        assert_eq!(reconstructed.pub_key_operator.scheme(), BlsSchemeKind::Legacy);
        assert_eq!(online.pub_key_operator.to_bytes(), [0u8; 48]);
    }

    /// Keys compare under the state's version scheme: one point held under each scheme is the
    /// same stored record when the version selects the scheme both re-encode to.
    #[test]
    fn keys_compare_under_the_version_scheme() {
        let point = basic(&VALID).get();
        let mut a = basic_state();
        a.pub_key_operator = BlsLazyPk::from_value(point.clone(), BlsSchemeKind::Basic);
        let mut b = basic_state();
        b.pub_key_operator = BlsLazyPk::from_value(point, BlsSchemeKind::Legacy);
        assert!(a.same_record(&b));
        assert_eq!(b.pub_key_operator.scheme(), BlsSchemeKind::Legacy);
    }

    /// Opus-high M1: a key set to a failed decode's object (Core's `Set(invalidObj)`) and the
    /// default key both serialize as zeros, yet they are not the same key, so records holding them
    /// are not the same record. Serialization alone cannot tell them apart.
    #[test]
    fn a_key_set_to_a_failed_decode_is_not_the_default_key() {
        let failed = basic(&UNDECODABLE).get();
        assert!(!failed.is_valid());
        let mut set_invalid = basic_state();
        set_invalid.pub_key_operator = BlsLazyPk::from_value(failed, BlsSchemeKind::Basic);
        let mut default = basic_state();
        default.pub_key_operator = BlsLazyPk::new(BlsSchemeKind::Basic);
        assert_eq!(set_invalid.pub_key_operator.clone().to_bytes(), [0u8; 48]);
        assert_eq!(default.pub_key_operator.clone().to_bytes(), [0u8; 48]);
        assert!(!set_invalid.same_record(&default));
        assert!(!default.same_record(&set_invalid));
        // Each is still the same record as itself.
        assert!(set_invalid.same_record(&set_invalid.clone()));
        assert!(default.same_record(&default.clone()));
    }

    #[test]
    fn core_eq_has_cores_side_effect() {
        // The reason MnCold has no PartialEq: Core's comparison decodes the raw key, the decode
        // fails, and the key serializes as zeros from then on.
        let (raw, decoded) = pair();
        assert!(!raw.core_eq(&decoded));
        assert_eq!(raw.pub_key_operator.to_bytes(), [0u8; 48]);
    }

    /// O2a-EV: a state is compared on the fields its version stores. Two version-3 states that
    /// differ only in `scriptPayout` or the platform ports, or two older states that differ only
    /// in the early-period fields, are the same record. Core's in-memory diff of the version-3
    /// pair still carries `scriptPayout`, and two such diffs with different scripts are different
    /// records, because a diff writes every set field.
    #[test]
    fn states_compare_on_the_fields_their_version_stores() {
        let ext = |f: fn(&mut MnCold)| {
            let mut c = MnCold {
                version: 3,
                ..cold_state(1, MnType::Regular)
            };
            f(&mut c);
            c
        };
        let base = ext(|_| {});
        assert!(base.same_record(&ext(|c| c.script_payout = vec![0x51])));
        assert!(base.same_record(&ext(|c| c.platform_p2p_port = 26656)));
        assert!(!base.same_record(&ext(|c| c.early_penalty = 1)));
        let legacy = cold_state(1, MnType::Regular);
        let mut early = legacy.clone();
        early.early_penalty = 1;
        assert!(legacy.same_record(&early));
        let mut script = legacy.clone();
        script.script_payout = vec![0x51];
        assert!(!legacy.same_record(&script));

        let dmn = |script: &[u8]| {
            let script = script.to_vec();
            crate::testkit::dmn_with(1, MnType::Regular, move |_, c| {
                c.version = 3;
                c.script_payout = script;
            })
        };
        let (a, b, c) = (dmn(&[]), dmn(&[0x51]), dmn(&[0x52]));
        assert!(a.same_record(&b), "the states are the same record");
        let ab = MnStateDiff::between(&a, &b);
        assert_eq!(ab.fields, MnStateDiff::SCRIPT_PAYOUT, "Core's in-memory diff carries it");
        assert!(ab.same_record(&ab.clone()));
        assert!(!ab.same_record(&MnStateDiff::between(&a, &c)));
    }
}
