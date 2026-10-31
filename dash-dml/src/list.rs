//! `MnList`: one immutable, persistent masternode list (EV2 §2.4), Core's
//! `CDeterministicMNList` over `imbl` maps.
//!
//! The mutations are Core's `AddMN`/`UpdateMN`/`RemoveMN` with the same failure order and
//! all-or-nothing semantics (§2.5); they end in one choke point that maintains the secondary
//! indexes, the counts and the collision buckets. `core_order()` is Core's iteration order
//! (quirk 41). EV2b adds the payee functions and PoSe (§3.3, §3.4).

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, OnceLock};

use dash_primitives::{BlockHash, OutPoint, ServiceV2};
use dash_types::Numeric;
use imbl::OrdMap;

use crate::ProTxHash;
use crate::index::Indexes;
use crate::order;
use crate::record::SameRecord;
use crate::types::{Counts, Dmn, MnCold, MnHot, OperatorPoint, Provenance};
use crate::unique::{NetProp, Prop, StateKeys, UniqueKey, UniqueMap, key_id_prop};

/// A deterministic masternode list: the state after one block (Core `CDeterministicMNList`).
///
/// `Clone` is O(1) (one reference-count increment per map). Lists are values: a block's list is
/// built from its parent's without changing it, and a list has one content whatever path produced
/// it (built online, decoded from a snapshot, or reconstructed from diffs; EV2 §2.8).
///
/// Iteration orders: [`MnList::iter`] is memcmp order over `proTxHash`, which the canonical
/// encoding and the SML root use. Core's own iteration (`ForEachMN`, `protx list`,
/// `BuildSimplifiedDiff`, `GetMNByOperatorKey`) is immer's CHAMP order over
/// [`ProTxHash::immer_hash`], which [`MnList::core_order`] reproduces (quirk 41).
#[derive(Clone)]
pub struct MnList {
    /// Null while a list is being built (Core `SetBlockHash(uint256())`).
    block_hash: BlockHash,
    /// -1 for the pre-DIP3 default list.
    height: i32,
    /// `nTotalRegisteredCount`.
    total_registered: u32,
    /// Core's `mnMap`, in memcmp order.
    pub(crate) mns: OrdMap<ProTxHash, Shared>,
    /// Core's `mnInternalIdMap`.
    by_internal_id: OrdMap<u64, ProTxHash>,
    /// Core's `mnUniquePropertyMap`.
    unique: UniqueMap,
    /// The secondary indexes and `GetCounts()` (EV2 §2.4).
    indexes: Indexes,
    /// immer collision-node sequences for 64-bit hashes shared by two or more masternodes, front
    /// first (EV2 §2.7, invariant I-C). Empty on real networks. A deque, so that a key set into a
    /// bucket goes to the front in O(1) (a crafted list of keys sharing one hash is linear to
    /// build, not quadratic).
    collision_buckets: CollisionBuckets,
    /// `core_order()`, computed once per list content; every mutation resets it.
    core_order: OnceLock<Arc<[ProTxHash]>>,
    /// P1 ∧ P2 (EV2 §3.4): whether the indexed payee path equals Core's scan on this list.
    /// Computed from the list itself on first use, never inherited; every mutation and every
    /// position change resets it.
    pub(crate) fast_payee_ok: OnceLock<bool>,
    provenance: Provenance,
}

/// The collision buckets of a list, by 64-bit hash: the keys sharing it, front first.
pub(crate) type CollisionBuckets = OrdMap<u64, Arc<VecDeque<ProTxHash>>>;

/// A list entry. Equal iff it is the same allocation: what `imbl`'s diff compares, so
/// [`MnList::diff_to`] sees exactly the entries a mutation replaced (Core's `BuildDiff`
/// compares pointers first).
#[derive(Clone)]
pub(crate) struct Shared(pub(crate) Arc<Dmn>);

impl PartialEq for Shared {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// A failed `AddMN`, `UpdateMN`, `RemoveMN` or `ApplyDiff`: one variant per Core
/// `std::runtime_error` (`deterministicmns.cpp` v24:371-679). Inside a block build every one
/// becomes `failed-procspectxsinblock` (EV2 §3.9); [`MnListError::core_message`] is Core's text
/// for the `FAILURE!` log line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MnListError {
    /// `AddMN`: the proTxHash is in the list.
    DuplicateProTxHash(ProTxHash),
    /// `AddMN`: the internal id is in the list.
    DuplicateInternalId(u64),
    /// `AddMN`: the collateral's slot is taken (or the outpoint is null).
    DuplicateCollateral {
        pro_tx: ProTxHash,
        collateral: OutPoint,
    },
    /// `AddMN`: an address or domain slot is taken.
    DuplicateAddress {
        pro_tx: ProTxHash,
        address: String,
    },
    /// `AddMN`: a network entry is neither an address nor a domain.
    InvalidAddress(ProTxHash),
    /// `AddMN`: a share owner key's slot is taken (or the key is null).
    DuplicateShareOwner(ProTxHash),
    /// `AddMN`: the owner key's slot is taken (or the key is null).
    DuplicateOwner(ProTxHash),
    /// `AddMN`: the operator key's slot is taken.
    DuplicateOperator(ProTxHash),
    /// `AddMN`: the platformNodeID's slot is taken.
    DuplicatePlatformNodeId(ProTxHash),
    /// `UpdateMN`/`RemoveMN` (the `&str`, Core's `__func__`): no masternode with this proTxHash.
    NotFound(&'static str, ProTxHash),
    /// `UpdateMN`: the network entries could not be moved (`reason` is Core's: "internal
    /// error", "invalid address" or "duplicate (…)").
    UpdateAddresses {
        pro_tx: ProTxHash,
        reason: String,
    },
    /// `UpdateMN`: the new owner key's slot is taken.
    UpdateOwner(ProTxHash),
    /// `UpdateMN`: the new operator key's slot is taken.
    UpdateOperator(ProTxHash),
    /// `UpdateMN`: the new platformNodeID's slot is taken.
    UpdatePlatformNodeId(ProTxHash),
    /// `RemoveMN`: a property's slot is not the masternode's (`what` names it as Core does).
    Delete {
        pro_tx: ProTxHash,
        what: &'static str,
    },
    /// `RemoveMN`: a network entry is neither an address nor a domain.
    DeleteInvalidAddress(ProTxHash),
    /// `ApplyDiff`: a removed internal id is not in the list.
    RemovedNotFound(u64),
    /// `ApplyDiff`: an updated internal id is not in the list.
    UpdatedNotFound(u64),
    /// A record's collision trailer (EV2 §2.9) does not hold exactly the list's keys with this
    /// 64-bit hash. Not a Core error: our records only.
    BadCollisionTrailer(u64),
    /// `AddMN`/`UpdateMN`: the state holds a field its version cannot store as it is (`field`,
    /// Core's name; [`MnCold::version_hidden_field`]): one the version does not serialize, set,
    /// `netInfo` in the other form, or a non-default operator key captured under the other
    /// scheme. Not a Core error: Core never writes such a state, and our records could not
    /// rebuild it (EV2a fix r1, D17).
    VersionHiddenField {
        pro_tx: ProTxHash,
        version: i32,
        field: &'static str,
    },
}

impl MnListError {
    /// Core's exception text (`what()`), up to the rendering of keys and addresses: key ids and
    /// operator keys are hex here, where Core prints a Base58 address and the key's string form
    /// (log text only, never consensus).
    pub fn core_message(&self) -> String {
        match self {
            MnListError::DuplicateProTxHash(p) => {
                format!("AddMN: Can't add a masternode with a duplicate proTxHash={p}")
            }
            MnListError::DuplicateInternalId(id) => {
                format!("AddMN: Can't add a masternode with a duplicate internalId={id}")
            }
            MnListError::DuplicateCollateral {
                pro_tx,
                collateral,
            } => format!(
                "AddMN: Can't add a masternode {pro_tx} with a duplicate collateralOutpoint={}-{}",
                collateral.hash, collateral.index
            ),
            MnListError::DuplicateAddress {
                pro_tx,
                address,
            } => {
                format!("AddMN: Can't add a masternode {pro_tx} with a duplicate address={address}")
            }
            MnListError::InvalidAddress(p) => {
                format!("AddMN: Can't add a masternode {p} with invalid address")
            }
            MnListError::DuplicateShareOwner(p) => {
                format!("AddMN: Can't add a masternode {p} with a duplicate share ownerKeyID")
            }
            MnListError::DuplicateOwner(p) => {
                format!("AddMN: Can't add a masternode {p} with a duplicate keyIDOwner")
            }
            MnListError::DuplicateOperator(p) => {
                format!("AddMN: Can't add a masternode {p} with a duplicate pubKeyOperator")
            }
            MnListError::DuplicatePlatformNodeId(p) => {
                format!("AddMN: Can't add a masternode {p} with a duplicate platformNodeID")
            }
            MnListError::NotFound(func, p) => {
                format!("{func}: Can't find a masternode with proTxHash={p}")
            }
            MnListError::UpdateAddresses {
                pro_tx,
                reason,
            } => format!(
                "UpdateMN: Can't update masternode {pro_tx} with addresses, reason={reason}"
            ),
            MnListError::UpdateOwner(p) => {
                format!("UpdateMN: Can't update a masternode {p} with a duplicate keyIDOwner")
            }
            MnListError::UpdateOperator(p) => {
                format!("UpdateMN: Can't update a masternode {p} with a duplicate pubKeyOperator")
            }
            MnListError::UpdatePlatformNodeId(p) => {
                format!("UpdateMN: Can't update a masternode {p} with a duplicate platformNodeID")
            }
            MnListError::Delete {
                pro_tx,
                what,
            } => {
                let article = if *what == "address" {
                    "an"
                } else {
                    "a"
                };
                format!("RemoveMN: Can't delete a masternode {pro_tx} with {article} {what}")
            }
            MnListError::DeleteInvalidAddress(p) => {
                format!("RemoveMN: Can't delete a masternode {p} with invalid address")
            }
            MnListError::RemovedNotFound(id) => {
                format!("ApplyDiff: can't find a removed masternode, id={id}")
            }
            MnListError::UpdatedNotFound(id) => {
                format!("ApplyDiff: can't find an updated masternode, id={id}")
            }
            MnListError::BadCollisionTrailer(hash) => {
                format!("collision trailer for hash {hash:#018x} differs from the list's keys")
            }
            MnListError::VersionHiddenField {
                pro_tx,
                version,
                field,
            } => format!("masternode {pro_tx}: state version {version} cannot store its {field}"),
        }
    }
}

impl fmt::Display for MnListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.core_message())
    }
}

impl std::error::Error for MnListError {}

/// A broken list invariant (EV2 §2.4): a value, never a panic, on lists from decoded data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvariantViolation(pub &'static str);

impl fmt::Display for InvariantViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MN list invariant {} broken", self.0)
    }
}

impl std::error::Error for InvariantViolation {}

impl MnList {
    /// `CDeterministicMNList()`: the pre-DIP3 default list (null hash, height -1, counter 0).
    pub fn default_pre_dip3() -> MnList {
        MnList {
            block_hash: BlockHash::ZERO,
            height: -1,
            total_registered: 0,
            mns: OrdMap::new(),
            by_internal_id: OrdMap::new(),
            unique: UniqueMap::default(),
            indexes: Indexes::default(),
            collision_buckets: OrdMap::new(),
            core_order: OnceLock::new(),
            fast_payee_ok: OnceLock::new(),
            provenance: Provenance::FromGenesis,
        }
    }

    /// An empty list for LQ1b's `MnsUsedAtH` sets (quirk 28: `AddMN` failures are swallowed),
    /// Core's `CDeterministicMNList()`.
    pub fn scratch() -> MnList {
        Self::default_pre_dip3()
    }

    /// `CDeterministicMNList(blockHash, height, totalRegisteredCount)`: an empty list at a
    /// position, as decoders and Core's initial snapshot build it. Its provenance is
    /// [`Provenance::Decoded`]: nothing proves its premises.
    pub fn new(block_hash: BlockHash, height: i32, total_registered: u32) -> MnList {
        MnList {
            block_hash,
            height,
            total_registered,
            provenance: Provenance::Decoded,
            ..Self::default_pre_dip3()
        }
    }

    /// `SetBlockHash(h)`.
    pub fn with_block_hash(mut self, block_hash: BlockHash) -> Self {
        self.block_hash = block_hash;
        self
    }

    /// `SetHeight(h)`.
    pub fn with_height(mut self, height: i32) -> Self {
        self.height = height;
        self.fast_payee_ok = OnceLock::new();
        self
    }

    /// `GetBlockHash()`.
    pub fn block_hash(&self) -> &BlockHash {
        &self.block_hash
    }

    /// `GetHeight()`.
    pub fn height(&self) -> i32 {
        self.height
    }

    /// `GetTotalRegisteredCount()`.
    pub fn total_registered(&self) -> u32 {
        self.total_registered
    }

    /// `GetCounts()`.
    pub fn counts(&self) -> Counts {
        self.indexes.counts
    }

    /// `GetAllMNsCount()`.
    pub fn len(&self) -> usize {
        self.mns.len()
    }

    /// Whether the list holds no masternode.
    pub fn is_empty(&self) -> bool {
        self.mns.is_empty()
    }

    /// Whether this equals Core's default `CDeterministicMNList()` (`deterministicmns.h`
    /// v24:618-624): null hash, height -1, no masternodes and a zero registration counter (the
    /// internal-id and unique-property maps are empty with the masternodes).
    pub fn is_default(&self) -> bool {
        self.block_hash.is_null()
            && self.height == -1
            && self.total_registered == 0
            && self.mns.is_empty()
    }

    /// Where the list came from (EV2 rev4): the no-panic rule depends on it.
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }

    /// `GetMN(proTxHash)`.
    pub fn get(&self, pro_tx: &ProTxHash) -> Option<&Arc<Dmn>> {
        self.mns.get(pro_tx).map(|s| &s.0)
    }

    /// `GetValidMN(proTxHash)`: the masternode if it is not PoSe-banned.
    pub fn get_valid(&self, pro_tx: &ProTxHash) -> Option<&Arc<Dmn>> {
        self.get(pro_tx).filter(|dmn| dmn.is_valid())
    }

    /// `IsMNValid(proTxHash)`: false when absent.
    pub fn is_valid(&self, pro_tx: &ProTxHash) -> bool {
        self.get_valid(pro_tx).is_some()
    }

    /// `IsMNPoSeBanned(proTxHash)`: false when absent.
    pub fn is_pose_banned(&self, pro_tx: &ProTxHash) -> bool {
        self.get(pro_tx).is_some_and(|dmn| dmn.hot.is_banned())
    }

    /// `GetMNByInternalId(internalId)`.
    pub fn get_by_internal_id(&self, id: u64) -> Option<&Arc<Dmn>> {
        self.by_internal_id.get(&id).and_then(|pro_tx| self.get(pro_tx))
    }

    /// `HasUniqueProperty(v)`.
    pub fn has_unique(&self, key: &UniqueKey) -> bool {
        self.unique.contains(key)
    }

    /// `GetUniquePropertyMN(v)`: the owner of the slot.
    pub fn unique_owner(&self, key: &UniqueKey) -> Option<&Arc<Dmn>> {
        self.unique.get(key).and_then(|slot| self.get(&slot.owner))
    }

    /// Core's `mnUniquePropertyMap`.
    #[cfg(any(test, feature = "xcheck"))]
    pub(crate) fn unique_map(&self) -> &UniqueMap {
        &self.unique
    }

    /// `GetMNByCollateral(collateralOutpoint)`: the owner of the outpoint's unique slot, Core's
    /// semantics. The spend pass adds Core's `collateralOutpoint == prevout` check itself
    /// (EV2 §3.5).
    pub fn get_by_collateral(&self, op: &OutPoint) -> Option<&Arc<Dmn>> {
        self.unique_owner(&UniqueKey::collateral(op))
    }

    /// `GetValidMNByCollateral(collateralOutpoint)`.
    pub fn get_valid_by_collateral(&self, op: &OutPoint) -> Option<&Arc<Dmn>> {
        self.get_by_collateral(op).filter(|dmn| dmn.is_valid())
    }

    /// `GetMNByService(service)`: the owner of the service's unique slot.
    pub fn get_by_service(&self, service: &ServiceV2) -> Option<&Arc<Dmn>> {
        self.unique_owner(&UniqueKey::service(service))
    }

    /// `GetMNByOperatorKey(pubKey)` (`deterministicmns.cpp` v24:93-102): the first masternode in
    /// [`MnList::core_order`] whose operator key decodes to `key`, bucket order included (two
    /// masternodes can hold one point under the two encodings).
    ///
    /// Like Core it decodes the stored keys it visits (`Get()`), so a key that fails to decode
    /// serializes as zeros afterwards; an invalid or null query matches the first masternode
    /// with a default key, as Core's `Get()` yields one invalid object for both (EV2 §2.6).
    // Core: evo/deterministicmns.cpp:93 CDeterministicMNList::GetMNByOperatorKey
    pub fn get_by_operator_key(&self, key: &OperatorPoint) -> Option<&Arc<Dmn>> {
        self.core_order()
            .iter()
            .filter_map(|pro_tx| self.get(pro_tx))
            .find(|dmn| dmn.cold.pub_key_operator.get() == *key)
    }

    /// `HasOperatorKeyUnderAnyScheme(pubkey, self)` (`deterministicmns.h` v24:471-482; V24 only,
    /// EVO-13): whether a masternode other than `except` holds `key`'s slot under either
    /// encoding. An invalid key is never held. Callers gate it on V24 where Core does (EV2 §3.7
    /// erratum; the ProUpSharedRegTx probe, `specialtxman.cpp` v24:688, is ungated); EV2b D11
    /// keeps the gates at the call sites.
    pub fn has_operator_key_under_any_scheme(
        &self,
        key: &OperatorPoint,
        except: &ProTxHash,
    ) -> bool {
        crate::build::BuildList::has_operator_key_under_any_scheme(self, key, except)
    }

    /// Every masternode, banned included, in memcmp order over `proTxHash` (the canonical
    /// encoding's and the SML root's order; **not** Core's iteration order).
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Dmn>> {
        self.mns.values().map(|s| &s.0)
    }

    /// The valid masternodes, in memcmp order.
    pub fn iter_valid(&self) -> impl Iterator<Item = &Arc<Dmn>> {
        self.iter().filter(|dmn| dmn.is_valid())
    }

    /// Core's iteration order (`ForEachMN`; quirk 41, EV2 §2.7): immer's CHAMP order over the
    /// 64-bit `ReadLE64(proTxHash)`, collision buckets most-recently-set first. Computed once per
    /// list content.
    pub fn core_order(&self) -> Arc<[ProTxHash]> {
        Arc::clone(
            self.core_order
                .get_or_init(|| order::core_order(self.mns.keys(), &self.collision_buckets).into()),
        )
    }

    /// The collision buckets (EV2 §2.7), front first: what the record trailers carry.
    pub fn collision_buckets(&self) -> impl Iterator<Item = (u64, &VecDeque<ProTxHash>)> {
        self.collision_buckets.iter().map(|(hash, bucket)| (*hash, &**bucket))
    }

    /// Every collision bucket, by hash.
    pub(crate) fn collision_buckets_map(&self) -> &CollisionBuckets {
        &self.collision_buckets
    }

    /// The bucket of `hash`, front first, if two or more masternodes share it.
    pub(crate) fn collision_bucket(&self, hash: u64) -> Option<&Arc<VecDeque<ProTxHash>>> {
        self.collision_buckets.get(&hash)
    }

    /// The payee queue (EV2 §2.4): the valid masternodes in `CompareByLastPaid` order.
    pub fn payee_queue(&self) -> impl DoubleEndedIterator<Item = &crate::PayeeKey> {
        self.indexes.payee_queue.iter()
    }

    /// The payee-queue keys whose pay height is at least `height`, in queue order.
    pub(crate) fn payee_queue_from(&self, height: i32) -> impl Iterator<Item = &crate::PayeeKey> {
        let from = crate::PayeeKey {
            height,
            pro_tx: ProTxHash::from([0u8; 32]),
        };
        self.indexes.payee_queue.range(from..)
    }

    /// Valid masternodes with a PoSe penalty (`DecreaseScores`' candidates), memcmp order.
    pub fn decaying(&self) -> impl Iterator<Item = &ProTxHash> {
        self.indexes.decaying.iter()
    }

    /// Masternodes without a confirmed hash, by `(registered height, proTxHash)`.
    pub fn unconfirmed(&self) -> impl Iterator<Item = &(i32, ProTxHash)> {
        self.indexes.unconfirmed.iter()
    }

    /// Evo masternodes with a payment streak, memcmp order.
    pub fn evo_streaks(&self) -> impl Iterator<Item = &ProTxHash> {
        self.indexes.evo_streak.iter()
    }

    /// `AddMN(dmn, fBumpTotalCount)` (`deterministicmns.cpp` v24:439-516). All or nothing: on
    /// `Err` the list is unchanged. A list changed through this or the other public mutators is
    /// [`Provenance::Decoded`]: nothing proves its premises (EV2 rev4).
    ///
    /// Order and failures as in Core: proTxHash, internal id, collateral, every network entry,
    /// the owner key (every share's for a shared masternode), the operator key unless default,
    /// the platformNodeID for an Evo masternode unless null. "Duplicate" means the slot exists
    /// and another masternode owns it; the same owner adds a reference. With `bump_total` the
    /// counter becomes `max(internalId + 1, counter)`, truncated to `u32` as Core assigns it.
    ///
    /// A state outside Core's state domain, including a non-default operator key captured under
    /// the other scheme from the version's (see [`MnList::update`]), is refused first
    /// ([`MnListError::VersionHiddenField`]).
    // Core: evo/deterministicmns.cpp:439 CDeterministicMNList::AddMN
    pub fn try_add(&mut self, dmn: Arc<Dmn>, bump_total: bool) -> Result<(), MnListError> {
        self.add_mn(dmn, bump_total)?;
        self.mark_decoded();
        Ok(())
    }

    /// [`MnList::try_add`] for the build and the readers: the provenance stays.
    pub(crate) fn add_mn(&mut self, dmn: Arc<Dmn>, bump_total: bool) -> Result<(), MnListError> {
        let pro_tx = dmn.pro_tx_hash;
        in_state_domain(&pro_tx, &dmn.cold)?;
        if self.mns.contains_key(&pro_tx) {
            return Err(MnListError::DuplicateProTxHash(pro_tx));
        }
        if self.by_internal_id.contains_key(&dmn.internal_id) {
            return Err(MnListError::DuplicateInternalId(dmn.internal_id));
        }
        let mut unique = self.unique.clone();
        add_props(&mut unique, &dmn)?;
        self.unique = unique;
        self.by_internal_id.insert(dmn.internal_id, pro_tx);
        if bump_total {
            let next = dmn.internal_id.wrapping_add(1).max(u64::from(self.total_registered));
            self.total_registered = next as u32;
        }
        self.set_entry(None, Some(dmn));
        Ok(())
    }

    /// `UpdateMN(oldDmn, newState)` (`deterministicmns.cpp` v24:518-602): installs `hot` and
    /// `cold` as the masternode's state. All or nothing.
    ///
    /// Unique slots move as in Core: the network entries if the objects differ (every old one
    /// deleted, then every new one added), the owner key, the operator key, and for an Evo
    /// masternode the platformNodeID. A BLS key moves when its serialization changes, Core v24's
    /// rule (`deterministicmns.h` v24:589-600); a key whose bytes stay keeps its slot, which is
    /// also where v23's point rule leaves the map (EV2 §3.7 erratum).
    ///
    /// A new state outside Core's state domain is refused first
    /// ([`MnListError::VersionHiddenField`]). That includes a non-default operator key captured
    /// under the other scheme from its state's version (legacy iff version 1; Core's
    /// `SetStateVersion`): the unique slot is keyed by the captured bytes, while a stored record
    /// re-captures the key under the version's scheme, so such a key would give a rebuilt list a
    /// different slot from the online one (EV2a D17).
    // Core: evo/deterministicmns.cpp:518 CDeterministicMNList::UpdateMN
    pub fn update(
        &mut self,
        pro_tx: &ProTxHash,
        hot: MnHot,
        cold: Arc<MnCold>,
    ) -> Result<(), MnListError> {
        self.update_mn(pro_tx, hot, cold)?;
        self.mark_decoded();
        Ok(())
    }

    /// [`MnList::update`] for the build: the provenance stays.
    pub(crate) fn update_mn(
        &mut self,
        pro_tx: &ProTxHash,
        hot: MnHot,
        cold: Arc<MnCold>,
    ) -> Result<(), MnListError> {
        let old = Arc::clone(self.get(pro_tx).ok_or(MnListError::NotFound("UpdateMN", *pro_tx))?);
        if !Arc::ptr_eq(&old.cold, &cold) {
            in_state_domain(pro_tx, &cold)?;
            let mut unique = self.unique.clone();
            update_props(&mut unique, &old, &cold)?;
            self.unique = unique;
        }
        let new = Arc::new(Dmn {
            hot,
            cold,
            ..(*old).clone()
        });
        self.set_entry(Some(&old), Some(new));
        Ok(())
    }

    /// `UpdateMN` for a change of the hot fields only: no unique slot can move, so it fails only
    /// when the masternode is absent.
    pub fn update_hot(&mut self, pro_tx: &ProTxHash, hot: MnHot) -> Result<(), MnListError> {
        self.update_hot_mn(pro_tx, hot)?;
        self.mark_decoded();
        Ok(())
    }

    /// [`MnList::update_hot`] for the build: the provenance stays.
    pub(crate) fn update_hot_mn(
        &mut self,
        pro_tx: &ProTxHash,
        hot: MnHot,
    ) -> Result<(), MnListError> {
        let old = Arc::clone(self.get(pro_tx).ok_or(MnListError::NotFound("UpdateMN", *pro_tx))?);
        let new = Arc::new(Dmn {
            hot,
            ..(*old).clone()
        });
        self.set_entry(Some(&old), Some(new));
        Ok(())
    }

    /// `RemoveMN(proTxHash)` (`deterministicmns.cpp` v24:612-679): deletes the masternode's
    /// slots in `AddMN`'s order, then the masternode. All or nothing. The counter is never
    /// decremented.
    // Core: evo/deterministicmns.cpp:612 CDeterministicMNList::RemoveMN
    pub fn remove(&mut self, pro_tx: &ProTxHash) -> Result<(), MnListError> {
        self.remove_mn(pro_tx)?;
        self.mark_decoded();
        Ok(())
    }

    /// [`MnList::remove`] for the build: the provenance stays.
    pub(crate) fn remove_mn(&mut self, pro_tx: &ProTxHash) -> Result<(), MnListError> {
        let dmn = Arc::clone(self.get(pro_tx).ok_or(MnListError::NotFound("RemoveMN", *pro_tx))?);
        let mut unique = self.unique.clone();
        delete_props(&mut unique, &dmn)?;
        self.unique = unique;
        self.by_internal_id.remove(&dmn.internal_id);
        self.set_entry(Some(&dmn), None);
        Ok(())
    }

    /// The choke point (EV2 §2.5): replaces `old` by `new` in the map, the indexes, the counts
    /// and the collision buckets, and resets the cached order. Every `set` of a key that shares
    /// its 64-bit hash moves it to the front of its bucket; a removal drops it (immer's
    /// collision-node rules, EV2 §2.7).
    fn set_entry(&mut self, old: Option<&Arc<Dmn>>, new: Option<Arc<Dmn>>) {
        if let Some(old) = old {
            self.indexes.remove(old);
        }
        let pro_tx = match (&new, old) {
            (Some(new), _) => new.pro_tx_hash,
            (None, Some(old)) => old.pro_tx_hash,
            (None, None) => return,
        };
        match new {
            Some(new) => {
                self.indexes.insert(&new);
                let replaced = self.mns.insert(pro_tx, Shared(new)).is_some();
                self.bucket_set(&pro_tx, replaced);
            }
            None => {
                self.mns.remove(&pro_tx);
                self.bucket_remove(&pro_tx);
            }
        }
        self.core_order = OnceLock::new();
        self.fast_payee_ok = OnceLock::new();
    }

    /// The keys of the list sharing `pro_tx`'s 64-bit hash, `pro_tx` included if present.
    fn same_hash(&self, pro_tx: &ProTxHash) -> impl Iterator<Item = &ProTxHash> {
        self.with_hash(pro_tx.immer_hash())
    }

    /// The keys of the list whose 64-bit hash (`ReadLE64` of the first 8 bytes) is `hash`, in
    /// memcmp order.
    fn with_hash(&self, hash: u64) -> impl Iterator<Item = &ProTxHash> {
        let (mut lo, mut hi) = ([0u8; 32], [0xffu8; 32]);
        lo[..8].copy_from_slice(&hash.to_le_bytes());
        hi[..8].copy_from_slice(&hash.to_le_bytes());
        self.mns.range(ProTxHash::from(lo)..=ProTxHash::from(hi)).map(|(k, _)| k)
    }

    /// `mnMap.set(k)` in a collision node: a new key is prepended, an existing one moves to the
    /// front (`node.hpp` `copy_collision_insert`/`copy_collision_replace`). `replaced` says the
    /// key was already in the map.
    ///
    /// A bucket exists exactly while two or more keys share the hash (I-C), so a key set into
    /// an existing bucket needs no scan of the map, and a new one is pushed to the front in
    /// place: constant time (amortised, and while the bucket is not shared with another list).
    /// Only a replaced key is searched for, which costs the bucket's length.
    fn bucket_set(&mut self, pro_tx: &ProTxHash, replaced: bool) {
        let hash = pro_tx.immer_hash();
        if let Some(bucket) = self.collision_buckets.get_mut(&hash) {
            let bucket = Arc::make_mut(bucket);
            if replaced && let Some(at) = bucket.iter().position(|k| k == pro_tx) {
                bucket.remove(at);
            }
            bucket.push_front(*pro_tx);
            return;
        }
        let mut sharing = self.same_hash(pro_tx).filter(|k| *k != pro_tx);
        let Some(first_other) = sharing.next().copied() else {
            return;
        };
        // A second key with this hash: the first one becomes the bucket's tail.
        // More than two sharing one hash is not reachable while I-C holds; keep the members
        // in memcmp order.
        let mut bucket = VecDeque::from([*pro_tx, first_other]);
        bucket.extend(sharing.copied());
        self.collision_buckets.insert(hash, Arc::new(bucket));
    }

    /// `mnMap.erase(k)` in a collision node: the rest keep their sequence; a single survivor is
    /// re-inlined (`champ.hpp` `do_sub`). Finding the key costs the bucket's length.
    fn bucket_remove(&mut self, pro_tx: &ProTxHash) {
        let hash = pro_tx.immer_hash();
        let Some(bucket) = self.collision_buckets.get_mut(&hash) else {
            return;
        };
        let at = bucket.iter().position(|k| k == pro_tx);
        if bucket.len() - usize::from(at.is_some()) < 2 {
            self.collision_buckets.remove(&hash);
        } else if let Some(at) = at {
            Arc::make_mut(bucket).remove(at);
        }
    }

    /// The same content marked [`Provenance::Decoded`] (EV2 rev4: a list from decoded or
    /// imported data, or built from one).
    pub(crate) fn mark_decoded(&mut self) {
        self.provenance = Provenance::Decoded;
    }

    /// Replaces the collision buckets of the hashes in `buckets` (a record trailer, EV2 §2.9). A
    /// sequence of two or more members must be a permutation of the list's keys with its hash;
    /// an empty one drops the bucket and is allowed only when fewer than two keys have the hash. A
    /// one-member sequence is never written. Anything else is [`MnListError::BadCollisionTrailer`]
    /// and leaves the list unchanged.
    pub(crate) fn restore_buckets<'a>(
        &mut self,
        buckets: impl Iterator<Item = (u64, &'a [ProTxHash])>,
    ) -> Result<(), MnListError> {
        let mut next = self.collision_buckets.clone();
        for (hash, sequence) in buckets {
            let keys: Vec<ProTxHash> = self.with_hash(hash).copied().collect();
            if sequence.is_empty() {
                if keys.len() >= 2 {
                    return Err(MnListError::BadCollisionTrailer(hash));
                }
                next.remove(&hash);
                continue;
            }
            let mut sorted = sequence.to_vec();
            sorted.sort();
            if sequence.len() < 2 || sorted != keys {
                return Err(MnListError::BadCollisionTrailer(hash));
            }
            next.insert(hash, Arc::new(sequence.iter().copied().collect()));
        }
        self.collision_buckets = next;
        self.core_order = OnceLock::new();
        Ok(())
    }

    /// Sets the counter (a record's `total_registered_after`, EV2 §2.8).
    pub(crate) fn set_total_registered(&mut self, total: u32) {
        self.total_registered = total;
    }

    /// Sets the position (Core `ApplyDiff`'s first two lines).
    pub(crate) fn set_position(&mut self, block_hash: BlockHash, height: i32) {
        self.block_hash = block_hash;
        self.height = height;
        self.fast_payee_ok = OnceLock::new();
    }

    /// Core's `IsEqual` (`deterministicmns.h` v24:487-525): position, unique map and sizes, and
    /// every masternode's fields except `internalId`, states compared by their serialization.
    /// The counter and the internal ids are not compared (Core calls them non-deterministic;
    /// EV2 §2.8).
    // Core: evo/deterministicmns.h:487 CDeterministicMNList::IsEqual
    pub fn is_equal(&self, rhs: &MnList) -> bool {
        self.block_hash == rhs.block_hash
            && self.height == rhs.height
            && self.unique == rhs.unique
            && self.mns.len() == rhs.mns.len()
            && self.by_internal_id.len() == rhs.by_internal_id.len()
            && self.mns.iter().all(|(pro_tx, a)| {
                rhs.mns.get(pro_tx).is_some_and(|b| {
                    let (a, b) = (&a.0, &b.0);
                    a.pro_tx_hash == b.pro_tx_hash
                        && a.collateral == b.collateral
                        && a.operator_reward == b.operator_reward
                        && a.mn_type == b.mn_type
                        && crate::codec::state_bytes(a) == crate::codec::state_bytes(b)
                })
            })
    }

    /// Invariants I-U, I-X, I-C and I-P (EV2 §2.4) and I-S (every state in Core's state domain,
    /// EV2a fix r1), recomputed from the masternodes. A violation is a value: lists from decoded
    /// data must never panic here (EV2 rev4).
    pub fn check_invariants(&self) -> Result<(), InvariantViolation> {
        // I-U: the unique map is exactly the masternodes' properties with their references.
        let mut unique = UniqueMap::default();
        for dmn in self.iter() {
            add_props(&mut unique, dmn).map_err(|_| InvariantViolation("I-U"))?;
        }
        if unique != self.unique {
            return Err(InvariantViolation("I-U"));
        }
        let ids: OrdMap<u64, ProTxHash> =
            self.iter().map(|dmn| (dmn.internal_id, dmn.pro_tx_hash)).collect();
        if ids != self.by_internal_id || ids.len() != self.mns.len() {
            return Err(InvariantViolation("I-U (internal ids)"));
        }
        if self.mns.iter().any(|(k, dmn)| *k != dmn.0.pro_tx_hash) {
            return Err(InvariantViolation("I-U (keys)"));
        }
        // I-S: every state is in Core's state domain.
        if self.iter().any(|dmn| dmn.cold.version_hidden_field().is_some()) {
            return Err(InvariantViolation("I-S"));
        }
        // I-X: every index equals its definition.
        if Indexes::of(self.iter().map(|d| &**d)) != self.indexes {
            return Err(InvariantViolation("I-X"));
        }
        // I-C: one bucket per shared 64-bit hash, holding exactly the keys with that hash.
        let mut shared: OrdMap<u64, Vec<ProTxHash>> = OrdMap::new();
        for k in self.mns.keys() {
            shared.entry(k.immer_hash()).or_default().push(*k);
        }
        let shared: OrdMap<u64, Vec<ProTxHash>> =
            shared.into_iter().filter(|(_, v)| v.len() > 1).collect();
        if shared.len() != self.collision_buckets.len() {
            return Err(InvariantViolation("I-C"));
        }
        for (hash, members) in &shared {
            let Some(bucket) = self.collision_buckets.get(hash) else {
                return Err(InvariantViolation("I-C"));
            };
            let mut sorted: Vec<ProTxHash> = bucket.iter().copied().collect();
            sorted.sort();
            if sorted != *members {
                return Err(InvariantViolation("I-C"));
            }
        }
        // I-P (EV2 §2.4, §3.4): P1 by a literal `ForEachMNShared` count, P2 by a scan, against
        // the lazily computed flag; P1 itself is asserted only on `FromGenesis` lists.
        let paid = self.iter_valid().filter(|d| d.hot.last_paid_height == self.height).count();
        let p2 = self.height > 0 || self.iter().all(|d| d.cold.registered_height >= 0);
        if self.fast_payee_ok() != (p2 && paid <= 1) {
            return Err(InvariantViolation("I-P (fast payee flag)"));
        }
        if self.provenance == Provenance::FromGenesis && self.height > 0 && paid > 1 {
            return Err(InvariantViolation("I-P"));
        }
        Ok(())
    }
}

/// Refuses a state outside Core's state domain ([`MnCold::version_hidden_field`]).
pub(crate) fn in_state_domain(pro_tx: &ProTxHash, cold: &MnCold) -> Result<(), MnListError> {
    match cold.version_hidden_field() {
        None => Ok(()),
        Some(field) => Err(MnListError::VersionHiddenField {
            pro_tx: *pro_tx,
            version: cold.version,
            field,
        }),
    }
}

/// `AddMN`'s unique-property steps (3)-(7) for `dmn`, in Core's order.
pub(crate) fn add_props(unique: &mut UniqueMap, dmn: &Dmn) -> Result<(), MnListError> {
    let pro_tx = dmn.pro_tx_hash;
    let collateral = Prop::collateral(&dmn.collateral);
    if !unique.add(&collateral, &pro_tx) {
        return Err(MnListError::DuplicateCollateral {
            pro_tx,
            collateral: dmn.collateral,
        });
    }
    let keys = dmn.cold.unique_keys();
    for entry in keys.net.iter() {
        match entry {
            NetProp::Addr {
                prop,
                shown,
            } => {
                if !unique.add(prop, &pro_tx) {
                    return Err(MnListError::DuplicateAddress {
                        pro_tx,
                        address: shown.clone(),
                    });
                }
            }
            NetProp::Invalid => return Err(MnListError::InvalidAddress(pro_tx)),
        }
    }
    for owner in keys.owners.iter() {
        if !unique.add(owner, &pro_tx) {
            return Err(if dmn.cold.is_shared() {
                MnListError::DuplicateShareOwner(pro_tx)
            } else {
                MnListError::DuplicateOwner(pro_tx)
            });
        }
    }
    if !keys.operator.null && !unique.add(&keys.operator, &pro_tx) {
        return Err(MnListError::DuplicateOperator(pro_tx));
    }
    if dmn.is_evo() && !keys.platform.null && !unique.add(&keys.platform, &pro_tx) {
        return Err(MnListError::DuplicatePlatformNodeId(pro_tx));
    }
    Ok(())
}

/// `RemoveMN`'s unique-property steps, in Core's order.
fn delete_props(unique: &mut UniqueMap, dmn: &Dmn) -> Result<(), MnListError> {
    let pro_tx = dmn.pro_tx_hash;
    let fail = |what| MnListError::Delete {
        pro_tx,
        what,
    };
    let collateral = Prop::collateral(&dmn.collateral);
    if !unique.delete(&collateral, &pro_tx) {
        return Err(fail("collateralOutpoint"));
    }
    let keys = dmn.cold.unique_keys();
    for entry in keys.net.iter() {
        match entry {
            NetProp::Addr {
                prop,
                ..
            } => {
                if !unique.delete(prop, &pro_tx) {
                    return Err(fail("address"));
                }
            }
            NetProp::Invalid => return Err(MnListError::DeleteInvalidAddress(pro_tx)),
        }
    }
    let shared = dmn.cold.is_shared();
    for owner in keys.owners.iter() {
        if !unique.delete(owner, &pro_tx) {
            return Err(fail(if shared {
                "share ownerKeyID"
            } else {
                "keyIDOwner"
            }));
        }
    }
    if !keys.operator.null && !unique.delete(&keys.operator, &pro_tx) {
        return Err(fail("pubKeyOperator"));
    }
    if dmn.is_evo() && !keys.platform.null && !unique.delete(&keys.platform, &pro_tx) {
        return Err(fail("platformNodeID"));
    }
    Ok(())
}

/// `UpdateMN`'s unique-property steps from `old`'s state to `new`.
fn update_props(unique: &mut UniqueMap, old: &Dmn, new: &MnCold) -> Result<(), MnListError> {
    let pro_tx = old.pro_tx_hash;
    let (before, after): (&StateKeys, &StateKeys) = (old.cold.unique_keys(), new.unique_keys());
    // Core's `shared_ptr_not_equal`: a deep comparison of the network objects.
    if !Arc::ptr_eq(&old.cold.net_info, &new.net_info) && old.cold.net_info != new.net_info {
        let fail = |reason: String| MnListError::UpdateAddresses {
            pro_tx,
            reason,
        };
        for entry in before.net.iter() {
            match entry {
                NetProp::Addr {
                    prop,
                    ..
                } if unique.delete(prop, &pro_tx) => {}
                NetProp::Addr {
                    ..
                } => return Err(fail("internal error".into())),
                NetProp::Invalid => return Err(fail("invalid address".into())),
            }
        }
        for entry in after.net.iter() {
            match entry {
                NetProp::Addr {
                    prop,
                    shown,
                } => {
                    if !unique.add(prop, &pro_tx) {
                        return Err(fail(format!("duplicate ({shown})")));
                    }
                }
                NetProp::Invalid => return Err(fail("invalid address".into())),
            }
        }
    }
    // keyIDOwner itself, also for a shared masternode (whose share keys never change).
    if old.cold.key_id_owner != new.key_id_owner {
        let prop = |cold: &MnCold| key_id_prop(&cold.key_id_owner);
        if !unique.replace(&prop(&old.cold), &prop(new), &pro_tx) {
            return Err(MnListError::UpdateOwner(pro_tx));
        }
    }
    if before.operator.key != after.operator.key
        && !unique.replace(&before.operator, &after.operator, &pro_tx)
    {
        return Err(MnListError::UpdateOperator(pro_tx));
    }
    if old.is_evo()
        && old.cold.platform_node_id != new.platform_node_id
        && !unique.replace(&before.platform, &after.platform, &pro_tx)
    {
        return Err(MnListError::UpdatePlatformNodeId(pro_tx));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-utils"))]
impl MnList {
    /// Test builder: Core's `AddMN(dmn, true)`, panicking on failure. The list becomes
    /// `Decoded`.
    ///
    /// # Panics
    ///
    /// When `try_add` fails.
    pub fn insert_for_tests(&mut self, dmn: Arc<Dmn>) {
        self.try_add(dmn, true).expect("test masternode must be addable");
        self.provenance = Provenance::Decoded;
    }
}

impl SameRecord for MnList {
    /// Full equality of our records (EV2 rev2, R2-m2): position, counter, every masternode
    /// (internal ids included) and the collision buckets, each masternode by its stored form. The
    /// unique map, the internal-id map and the indexes are functions of the masternodes
    /// (invariants I-U, I-X); the provenance is not content. Core's `IsEqual`, which ignores the
    /// counter and the internal ids, is `MnList::is_equal`.
    fn same_record(&self, other: &Self) -> bool {
        self.block_hash == other.block_hash
            && self.height == other.height
            && self.total_registered == other.total_registered
            && self.collision_buckets == other.collision_buckets
            && (self.mns.ptr_eq(&other.mns)
                || self.mns.len() == other.mns.len()
                    && self
                        .mns
                        .iter()
                        .zip(other.mns.iter())
                        .all(|((a_key, a), (b_key, b))| a_key == b_key && a.0.same_record(&b.0)))
    }
}

impl fmt::Debug for MnList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MnList")
            .field("block_hash", &self.block_hash)
            .field("height", &self.height)
            .field("total_registered", &self.total_registered)
            .field("len", &self.mns.len())
            .field("counts", &self.indexes.counts)
            .field("collision_buckets", &self.collision_buckets.len())
            .field("provenance", &self.provenance)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
