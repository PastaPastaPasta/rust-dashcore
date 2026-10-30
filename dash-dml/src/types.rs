//! A masternode as the list holds it (EV2 §2.3): Core's `CDeterministicMN` split into a `Copy` hot
//! part, which the per-block steps change, and a shared cold part.

use std::fmt;
use std::sync::{Arc, OnceLock};

use dash_evo::types::{ProTxHash, RawMnType};
use dash_num::Hash256;
use dash_pkc::bls::{BLS_PK_LEN, BlsLazyPk, BlsSchemeKind, BlsValue, G1};
use dash_pkc::ecdsa::EcdsaPkHash;
use dash_primitives::{
    AddrV1, CollateralShare, NetInfo, NetInfoV1, OutPoint, PayoutShare, PlatformNodeId, ServiceV1,
};

use crate::unique::StateKeys;

/// `ProTxVersion::ExtAddr`.
pub(crate) const EXT_ADDR: i32 = 3;

/// Whether a payload or target `version` is ExtAddr or later.
pub(crate) fn ext_addr(version: u16) -> bool {
    i32::from(version) >= EXT_ADDR
}

/// An owner or voting key id (Core `CKeyID`, 20 bytes).
pub type KeyId = EcdsaPkHash;

/// A stored operator key: Core's `CBLSLazyPublicKey`, raw bytes plus the scheme captured when it
/// was set, decoded on first use (BS11a-t). Its stored encoding is legacy iff the state's
/// `nVersion == 1` (`dmnstate.h` v24:107; never the global flag).
pub type OperatorKey = BlsLazyPk;

/// Whether `key` equals the default key of its own scheme. Comparing against the default object
/// never decodes (`bls.h` v24:514).
pub(crate) fn is_default_operator_key(key: &OperatorKey) -> bool {
    *key == OperatorKey::new(key.scheme())
}

/// A decoded operator key, what [`OperatorKey::get`] returns (Core `CBLSPublicKey`): the query
/// type of `GetMNByOperatorKey` and `HasOperatorKeyUnderAnyScheme`.
pub type OperatorPoint = BlsValue<G1, BLS_PK_LEN>;

/// A script held in a masternode state (`scriptPayout`, `scriptOperatorPayout`): Core's
/// `CScript` bytes. base-sdk does not export its script buffer type yet (open question in
/// `dash-evo/API.md` Q6).
pub type ScriptBytes = Vec<u8>;

/// The fields the per-block steps change without touching keys or addresses (EV2 §2.3): payee
/// bookkeeping, PoSe decay, punishment, bans and revivals. Core `int`s, with Core's sentinels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MnHot {
    /// `nLastPaidHeight` (0 = never).
    pub last_paid_height: i32,
    /// `nConsecutivePayments`.
    pub consecutive_payments: i32,
    /// `nPoSePenalty`.
    pub pose_penalty: i32,
    /// `nPoSeRevivedHeight` (-1 = never).
    pub pose_revived_height: i32,
    /// `nPoSeBanHeight` (-1 = valid). Private as in Core (`dmnstate.h` v24:36); read it with
    /// [`MnHot::banned_height`], change it with [`MnHot::ban_if_not_banned`] and
    /// [`MnHot::revive`]. The crate's codecs set it directly.
    pub(crate) pose_ban_height: i32,
}

impl Default for MnHot {
    /// Core's member initializers (`dmnstate.h` v24:36-48).
    fn default() -> Self {
        MnHot {
            last_paid_height: 0,
            consecutive_payments: 0,
            pose_penalty: 0,
            pose_revived_height: -1,
            pose_ban_height: -1,
        }
    }
}

impl MnHot {
    /// `IsBanned()`.
    // Core: evo/dmnstate.h:146 CDeterministicMNState::IsBanned
    pub fn is_banned(&self) -> bool {
        self.pose_ban_height != -1
    }

    /// `GetBannedHeight()`.
    // Core: evo/dmnstate.h:142 CDeterministicMNState::GetBannedHeight
    pub fn banned_height(&self) -> i32 {
        self.pose_ban_height
    }

    /// `BanIfNotBanned(height)`.
    // Core: evo/dmnstate.h:136 CDeterministicMNState::BanIfNotBanned
    pub fn ban_if_not_banned(&mut self, height: i32) {
        if !self.is_banned() {
            self.pose_ban_height = height;
        }
    }

    /// `Revive(nRevivedHeight)`.
    // Core: evo/dmnstate.h:150 CDeterministicMNState::Revive
    pub fn revive(&mut self, revived_height: i32) {
        self.pose_penalty = 0;
        self.pose_ban_height = -1;
        self.pose_revived_height = revived_height;
    }
}

/// The rest of Core's `CDeterministicMNState` (`dmnstate.h` v24:33-175), shared by every list
/// that holds an unchanged state. Never mutated in place: a change installs a new `Arc<MnCold>`.
///
/// **No `PartialEq`** (EV0 API review r1, M1). The operator key's `operator==` can decode it,
/// and a key that fails to decode serializes as zeros from then on, so an ordinary `==` on a
/// shared state could change what every list holding it serializes. Two comparisons exist
/// instead:
/// - [`MnCold::core_eq`]: Core's comparison, side effects included, for the places Core compares;
/// - [`crate::SameRecord::same_record`]: the record as Core serializes it, with the operator keys
///   also equal under Core's `operator==` on clones (the compared values never change), for tests
///   and the store's rewrite check.
///
/// Built in the crate (EV2a's `from_pro_reg` and codecs) or by `testkit::cold_state`.
#[derive(Clone, Debug)]
pub struct MnCold {
    /// `nVersion` (Core `int`, 4 bytes on disk); the operator key's stored encoding is legacy
    /// iff it is 1.
    pub version: i32,
    /// `nRegisteredHeight`.
    pub registered_height: i32,
    /// `nRevocationReason`.
    pub revocation_reason: u16,
    /// `confirmedHash`: the block hash `nMasternodeMinimumConfirmations` blocks after
    /// registration; null while unconfirmed.
    pub confirmed_hash: Hash256,
    /// `confirmedHashWithProRegTxHash`: single SHA256(proTxHash ‖ confirmedHash), stored, never
    /// recomputed by readers (LQ1 §2.2.1).
    pub confirmed_hash_with_pro_tx: Hash256,
    /// `keyIDOwner`; null for shared masternodes.
    pub key_id_owner: KeyId,
    /// `pubKeyOperator`.
    pub pub_key_operator: OperatorKey,
    /// `keyIDVoting`.
    pub key_id_voting: KeyId,
    /// `netInfo`: `MnNetInfo` for `nVersion < 3`, `ExtNetInfo` from 3.
    pub net_info: Arc<NetInfo>,
    /// `scriptPayout` (`nVersion < 3`).
    pub script_payout: ScriptBytes,
    /// `payouts` (`nVersion >= 3`): the owner payout shares.
    pub payouts: Vec<PayoutShare>,
    /// `shares` (`nVersion >= 3`): the collateral shares; non-empty ⇔ a shared masternode.
    pub shares: Vec<CollateralShare>,
    /// `nEarlyPeriodBlocks`.
    pub early_period_blocks: u32,
    /// `nEarlyPenalty` (Core `CAmount`).
    pub early_penalty: i64,
    /// `scriptOperatorPayout`.
    pub script_operator_payout: ScriptBytes,
    /// `platformNodeID`.
    pub platform_node_id: PlatformNodeId,
    /// `platformP2PPort` (`nVersion < 3`).
    pub platform_p2p_port: u16,
    /// `platformHTTPPort` (`nVersion < 3`).
    pub platform_http_port: u16,
    /// Values computed once per state. Private: a state built outside the crate cannot carry
    /// another state's cached values. In-crate code that builds a changed state from an old one
    /// clones it (which empties the cache) rather than moving the old `derived` along.
    pub(crate) derived: ColdDerived,
}

/// Values computed once per cold state (EV2 §2.3): empty when built, and empty again in a clone
/// (a cloned state is about to be changed). A cache, not content: never serialized, never
/// compared.
#[derive(Default)]
pub(crate) struct ColdDerived {
    /// The unique-property keys this state holds, built once from the serialization bytes
    /// (EV2 §2.5): no hashing, and none per `Add`/`Update`/`Remove` as in Core. It assumes a
    /// later decode does not change a stored key's identity (stored keys are valid: the
    /// importer's and the transaction checks' job).
    pub(crate) unique_keys: OnceLock<Arc<StateKeys>>,
}

impl Clone for ColdDerived {
    fn clone(&self) -> Self {
        ColdDerived::default()
    }
}

impl fmt::Debug for ColdDerived {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ColdDerived")
    }
}

/// The scheme an operator key is stored under for a state `version`: legacy iff `nVersion == 1`
/// (`ProTxVersion::LegacyBLS`, `dmnstate.h` v24:107), never the global flag.
pub(crate) fn scheme_for(version: i32) -> BlsSchemeKind {
    BlsSchemeKind::from_legacy(version == 1)
}

impl MnCold {
    /// Core's default `CDeterministicMNState` (`dmnstate.h` v24:36-70): the state a decoded
    /// `CDeterministicMNStateDiff` sets its fields over. Core's null `netInfo` is the empty
    /// legacy service here; no reader looks at a field the diff does not set.
    pub(crate) fn core_default() -> MnCold {
        MnCold {
            version: 1,
            registered_height: -1,
            revocation_reason: 0,
            confirmed_hash: Hash256::default(),
            confirmed_hash_with_pro_tx: Hash256::default(),
            key_id_owner: KeyId::default(),
            pub_key_operator: OperatorKey::new(BlsSchemeKind::Legacy),
            key_id_voting: KeyId::default(),
            net_info: Arc::new(NetInfo::Legacy(NetInfoV1(ServiceV1 {
                addr: AddrV1([0; 16]),
                port: 0,
            }))),
            script_payout: Vec::new(),
            payouts: Vec::new(),
            shares: Vec::new(),
            early_period_blocks: 0,
            early_penalty: 0,
            script_operator_payout: Vec::new(),
            platform_node_id: PlatformNodeId::default(),
            platform_p2p_port: 0,
            platform_http_port: 0,
            derived: ColdDerived::default(),
        }
    }

    /// Empties the derived caches: the state is about to be, or has just been, changed.
    pub(crate) fn reset_derived(&mut self) {
        self.derived = ColdDerived::default();
    }

    /// `nVersion` as the `uint16_t` it is in Core's state; ours holds it in an `i32`.
    pub(crate) fn state_version(&self) -> u16 {
        self.version as u16
    }

    /// `IsShared()`: whether the state holds collateral shares.
    pub fn is_shared(&self) -> bool {
        !self.shares.is_empty()
    }

    /// The scheme the state's operator key is stored under (`scheme_for` its version).
    pub fn key_scheme(&self) -> BlsSchemeKind {
        scheme_for(self.version)
    }

    /// `CBLSLazyPublicKeyVersionWrapper(key, legacy)`: the key under the scheme the version
    /// selects, re-encoded from a **clone** (never the stored key) if it was captured under the
    /// other one.
    pub(crate) fn operator_key_bytes(&self) -> [u8; 48] {
        self.pub_key_operator.clone().to_bytes_as(self.key_scheme())
    }

    /// Core's comparison of two states, every member in member order (`dmnstate.h` v24:39-66),
    /// as the state-diff constructor makes it, with the operator key compared by
    /// `CBLSLazyWrapper::operator==` (`bls/bls.h` v24:514), **which may decode both keys and
    /// leave an undecodable one serializing as zeros**. Use it only where Core compares;
    /// everything else uses [`crate::SameRecord::same_record`].
    ///
    /// Every member is compared even when an earlier one differs (EV0 r3 m2): Core's diff
    /// constructor visits each member, so the key comparison and its side effect happen whatever
    /// the other fields hold.
    pub fn core_eq(&self, other: &Self) -> bool {
        let key = self.pub_key_operator == other.pub_key_operator;
        key && self.version == other.version
            && self.registered_height == other.registered_height
            && self.revocation_reason == other.revocation_reason
            && self.confirmed_hash == other.confirmed_hash
            && self.confirmed_hash_with_pro_tx == other.confirmed_hash_with_pro_tx
            && self.key_id_owner == other.key_id_owner
            && self.key_id_voting == other.key_id_voting
            && self.net_info == other.net_info
            && self.script_payout == other.script_payout
            && self.payouts == other.payouts
            && self.shares == other.shares
            && self.early_period_blocks == other.early_period_blocks
            && self.early_penalty == other.early_penalty
            && self.script_operator_payout == other.script_operator_payout
            && self.platform_node_id == other.platform_node_id
            && self.platform_p2p_port == other.platform_p2p_port
            && self.platform_http_port == other.platform_http_port
    }

    /// `ResetOperatorFields()` (`dmnstate.h` v24:127-135): version 1, the default operator key,
    /// an empty legacy service, no operator payout, reason 0, a null platformNodeID. The platform
    /// ports are **not** reset, as in Core. `scheme` is the one Core's default key captures (the
    /// global flag; it serializes as zeros under either). A version-3 state keeps its payouts,
    /// shares and early-period fields, which version 1 hides: as in Core, the caller restores the
    /// version (`SetStateVersion`) before installing the state (see
    /// [`MnCold::version_hidden_field`]).
    pub fn reset_operator_fields(&mut self, scheme: BlsSchemeKind) {
        self.version = 1;
        self.pub_key_operator = OperatorKey::new(scheme);
        self.net_info = Arc::new(NetInfo::Legacy(NetInfoV1(ServiceV1 {
            addr: AddrV1([0; 16]),
            port: 0,
        })));
        self.script_operator_payout.clear();
        self.revocation_reason = 0;
        self.platform_node_id = PlatformNodeId::default();
        self.reset_derived();
    }

    /// The first field, by Core's name, that this state's version cannot store as it is: a
    /// field the version does not serialize left away from its default (`scriptPayout` and the
    /// platform ports from version 3 (ExtAddr), the payouts, shares and early-period fields below
    /// it; `dmnstate.h` v24:96-115), or `netInfo` in the other form from the one the version
    /// reads (`ExtNetInfo` exactly from version 3; `NetInfoSerWrapper`, `netinfo.h`
    /// v24:497-505), or a non-default `pubKeyOperator` captured under the other scheme from the
    /// one the version selects (EV2a D17; the payload readers capture it under the payload's
    /// version, `providertx.h` v24:180, :346, and `SetStateVersion` re-captures it,
    /// `specialtxman.cpp` v24:69-71; the default key, which `ResetOperatorFields` leaves under
    /// the global flag, serializes as zeros under either). Core's state domain: no Core write
    /// site leaves a state outside it (`SetStateVersion`, `ResetOperatorFields`' `MakeNetInfo(1)`
    /// and the payload readers; EV2a fix r1 and r2), and a record, which stores what the version
    /// serializes, could not rebuild one. [`MnList`](crate::MnList) refuses a state outside it.
    pub fn version_hidden_field(&self) -> Option<&'static str> {
        let extended = self.version >= EXT_ADDR;
        if matches!(*self.net_info, NetInfo::Extended(_)) != extended {
            return Some("netInfo");
        }
        // Comparing against the default object never decodes (`bls.h` v24:514).
        let key = &self.pub_key_operator;
        if key.scheme() != self.key_scheme() && *key != OperatorKey::new(self.key_scheme()) {
            return Some("pubKeyOperator");
        }
        if extended {
            [
                ("scriptPayout", !self.script_payout.is_empty()),
                ("platformP2PPort", self.platform_p2p_port != 0),
                ("platformHTTPPort", self.platform_http_port != 0),
            ]
            .into_iter()
            .find_map(|(name, set)| set.then_some(name))
        } else {
            [
                ("payouts", !self.payouts.is_empty()),
                ("shares", !self.shares.is_empty()),
                ("nEarlyPeriodBlocks", self.early_period_blocks != 0),
                ("nEarlyPenalty", self.early_penalty != 0),
            ]
            .into_iter()
            .find_map(|(name, set)| set.then_some(name))
        }
    }

    /// `UpdateConfirmedHash(proTxHash, confirmedHash)` (`dmnstate.h` v24:162-169): the hash and
    /// the **single** SHA256 of `proTxHash ‖ confirmedHash`.
    pub fn update_confirmed_hash(&mut self, pro_tx: &ProTxHash, confirmed: &Hash256) {
        let mut pre = [0u8; 64];
        pre[..32].copy_from_slice(pro_tx.as_bytes());
        pre[32..].copy_from_slice(confirmed.as_bytes());
        self.confirmed_hash = *confirmed;
        self.confirmed_hash_with_pro_tx = Hash256::from(dash_consensus::hash::sha256(&pre));
        self.reset_derived();
    }
}

/// One masternode in a list (Core `CDeterministicMN`, `evo/deterministicmns.h` v24:43-94).
/// Immutable:
/// a change installs a new `Arc<Dmn>`; unchanged masternodes keep their `Arc`s across blocks.
#[derive(Clone, Debug)]
pub struct Dmn {
    /// `proTxHash`.
    pub pro_tx_hash: ProTxHash,
    /// `internalId`: consensus-deterministic from genesis (`evo/snapshot.h` v24:229-236).
    pub internal_id: u64,
    /// `collateralOutpoint`.
    pub collateral: OutPoint,
    /// `nOperatorReward`, fixed at registration.
    pub operator_reward: u16,
    /// `nType`, raw: any `u16` Core's reader accepts (EV0 API review Q13).
    pub mn_type: RawMnType,
    /// The hot part of the state.
    pub hot: MnHot,
    /// The cold part of the state.
    pub cold: Arc<MnCold>,
}

impl Dmn {
    /// Whether the masternode is valid (`!IsBanned()`), Core's `IsValid`.
    pub fn is_valid(&self) -> bool {
        !self.hot.is_banned()
    }

    /// `nType == MnType::Evo`.
    pub fn is_evo(&self) -> bool {
        self.mn_type.is_evo()
    }

    /// The voting weight: 1, 4, or 0 for an undefined type.
    pub fn weight(&self) -> u32 {
        self.mn_type.weight()
    }

    /// `CompareByLastPaid_GetHeight` (`deterministicmns.cpp` v24:132-141): the revival height if
    /// it is set and above the last payment, else the last payment, or the registration height
    /// when never paid.
    // Core: evo/deterministicmns.cpp:132 CompareByLastPaid_GetHeight
    pub fn pay_height(&self) -> i32 {
        let height = self.hot.last_paid_height;
        if self.hot.pose_revived_height != -1 && self.hot.pose_revived_height > height {
            self.hot.pose_revived_height
        } else if height == 0 {
            self.cold.registered_height
        } else {
            height
        }
    }

    /// The payee-queue key (EV2 §2.4).
    pub fn payee_key(&self) -> PayeeKey {
        PayeeKey {
            height: self.pay_height(),
            pro_tx: self.pro_tx_hash,
        }
    }
}

/// The payee-queue key (EV2 §2.4): `(CompareByLastPaid_GetHeight, proTxHash)`, ordered as Core's
/// `CompareByLastPaid` with the memcmp tie-break (quirk 13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PayeeKey {
    /// The effective last-paid height (EV2 §3.4).
    pub height: i32,
    /// The tie-break.
    pub pro_tx: ProTxHash,
}

/// Core's `CDeterministicMNList::Counts` (`deterministicmns.h` v24:146-157), kept in O(1).
///
/// The weighted sums are fields, not derived: an undefined `nType` counts as regular with weight
/// 0, so they are not a function of the four counts. `u64` is Core's `size_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// `m_total_evo`.
    pub total_evo: u64,
    /// `m_total_mn`: every non-Evo masternode, undefined types included.
    pub total_mn: u64,
    /// `m_total_weighted`.
    pub total_weighted: u64,
    /// `m_valid_evo`.
    pub valid_evo: u64,
    /// `m_valid_mn`.
    pub valid_mn: u64,
    /// `m_valid_weighted`.
    pub valid_weighted: u64,
}

impl Counts {
    /// `total()`: every masternode, banned included.
    pub fn total(&self) -> u64 {
        self.total_mn + self.total_evo
    }

    /// `enabled()`: the valid (non-banned) masternodes.
    pub fn enabled(&self) -> u64 {
        self.valid_mn + self.valid_evo
    }

    /// Adds `dmn`: the body of `GetCounts()`'s loop. The list's choke point keeps the counts with
    /// it and with [`Counts::remove`].
    // Core: evo/deterministicmns.h:269 CDeterministicMNList::GetCounts
    pub(crate) fn add(&mut self, dmn: &Dmn) {
        self.step(dmn, true);
    }

    /// The inverse of [`Counts::add`] for a masternode the counts hold.
    pub(crate) fn remove(&mut self, dmn: &Dmn) {
        self.step(dmn, false);
    }

    fn step(&mut self, dmn: &Dmn, add: bool) {
        let is_valid = dmn.is_valid();
        let weight = u64::from(dmn.weight());
        let bump = |n: &mut u64, by: u64| {
            *n = if add {
                *n + by
            } else {
                *n - by
            }
        };
        if dmn.is_evo() {
            bump(&mut self.total_evo, 1);
            if is_valid {
                bump(&mut self.valid_evo, 1);
            }
        } else {
            bump(&mut self.total_mn, 1);
            if is_valid {
                bump(&mut self.valid_mn, 1);
            }
        }
        if is_valid {
            bump(&mut self.valid_weighted, weight);
        }
        bump(&mut self.total_weighted, weight);
    }
}

/// Where a list came from (EV2 rev4, DEC-58): the no-panic rule depends on it.
///
/// `FromGenesis` lists have proven temporal premises (`last_paid <= height`, registration and
/// revival heights not above the list height), so invariant checks may assert on them in test
/// and xcheck builds. `Decoded` lists (snapshots, imports, peer-supplied diffs, fixtures) and
/// every list built from one may hold anything Core's canonical reader accepts: **no invariant
/// check may panic or abort on them**; violations are values or typed errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Built block by block from the pre-DIP3 default list.
    FromGenesis,
    /// Decoded, imported, reconstructed from diffs, or built from such a list.
    Decoded,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MnType;

    #[test]
    fn hot_state_follows_core() {
        let mut hot = MnHot::default();
        assert!(!hot.is_banned());
        hot.ban_if_not_banned(10);
        hot.ban_if_not_banned(11);
        assert_eq!(hot.banned_height(), 10);
        hot.pose_penalty = 5;
        hot.revive(12);
        assert_eq!((hot.pose_penalty, hot.banned_height(), hot.pose_revived_height), (0, -1, 12));
    }

    #[test]
    fn undefined_types_count_as_regular_with_weight_zero() {
        // Core's GetCounts: anything not Evo is counted as regular, weighted by GetMnType.
        assert_eq!((RawMnType(7).weight(), RawMnType(7).is_evo()), (0, false));
        assert_eq!((RawMnType::EVO.weight(), RawMnType::REGULAR.weight()), (4, 1));
        let mut counts = Counts::default();
        for (n, t) in [(1, RawMnType::REGULAR), (2, RawMnType::EVO), (3, RawMnType(7))] {
            let mut dmn = (*crate::testkit::dmn(n, MnType::Regular)).clone();
            dmn.mn_type = t;
            counts.add(&dmn);
        }
        let mut banned = (*crate::testkit::dmn(4, MnType::Evo)).clone();
        banned.hot.ban_if_not_banned(10);
        counts.add(&banned);
        assert_eq!((counts.total(), counts.enabled()), (4, 3));
        assert_eq!((counts.total_mn, counts.total_evo, counts.valid_evo), (2, 2, 1));
        assert_eq!((counts.total_weighted, counts.valid_weighted), (9, 5));
    }
}
