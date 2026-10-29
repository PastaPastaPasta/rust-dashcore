//! The DML codecs (EV2 §2.9): Core's canonical list and diff encodings (`snapshot.h`
//! v24:237-394), `CanonicalMNListHash`, our record envelope, and Core's evodb encodings for the
//! importer ([`core_disk`]).
//!
//! The encoders are written here over [`CoreWrite`] (A1 §1.3: no consensus byte comes from
//! base-sdk's encoders); base-sdk's decoders read the network-address types. Every canonical
//! entry is decoded, re-encoded and compared with the bytes it was read from, Core's
//! `CheckCanonicalEncoding`, so whatever a decoder normalizes is refused rather than rewritten.

use std::fmt;
use std::sync::Arc;

use dash_consensus::primitives::{
    CoreWrite, HashWriter, read_compact_size, read_varint_u32, read_varint_u64, write_compact_size,
    write_varint,
};
use dash_num::Hash256;
use dash_pkc::bls::{BlsLazyPk, BlsSchemeSource};
use dash_primitives::{
    BlockHash, CollateralShare, NIEntry, NIEntryCode, NIPurpose, NITrait, NetInfo, NetInfoV1,
    NetInfoV2, OutPoint, PayoutShare, PlatformNodeId, TxHash,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use crate::diff::{MnListDiff, MnListDiffRecord, MnStateDiff};
use crate::list::MnList;
use crate::netinfo;
use crate::types::{ColdDerived, Dmn, EXT_ADDR, MnCold, MnHot, scheme_for};
use crate::{ProTxHash, RawMnType};

/// `EVO_SNAPSHOT_MAX_MNS`: the most masternodes a canonical list or diff section holds.
pub const MAX_MNS: usize = 100_000;
/// `EVO_SNAPSHOT_MAX_PAYOUT_SHARES`.
pub const MAX_PAYOUT_SHARES: usize = 8;
/// `EVO_SNAPSHOT_MAX_MN_COMPACT_ITEMS`: the per-entry budget of nested CompactSize values.
pub const MAX_MN_COMPACT_ITEMS: u64 = 10_000;
/// `EVO_SNAPSHOT_MAX_HASH_PREFIX_RUN`: the most entries sharing their leading 64 bits.
pub const MAX_HASH_PREFIX_RUN: usize = 8;
/// `ExtNetInfo::CURRENT_VERSION` (`netinfo.h` v24:378): Core reads nothing past a version of 0
/// or above it.
const EXT_NET_INFO_VERSION: u8 = 1;

/// Core's `netInfo->Validate() == NetInfoStatus::Success`, which the canonical reader and
/// `ValidateCanonicalMNInvariants` require of every non-empty network info. It depends on the
/// chain's parameters (the mainnet-port rule, `RequireRoutableExternalIP`), and EV1 ports it
/// (EV2 §1.3: NetInfo validation is EV1's), so the codec takes it from the caller.
pub trait NetInfoValidity {
    /// Whether Core's `Validate()` accepts `net_info`.
    fn is_valid(&self, net_info: &NetInfo) -> bool;
}

/// base-sdk's context-free `check()`: **not** Core's `Validate()` (no port or routability
/// rules), for tests over lists whose addresses Core accepted.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct ContextFreeNetInfo;

#[cfg(any(test, feature = "test-utils"))]
impl NetInfoValidity for ContextFreeNetInfo {
    fn is_valid(&self, net_info: &NetInfo) -> bool {
        dash_types::Checkable::check(net_info).is_none()
    }
}

/// A decode or encode refusal, with Core's `ios_base::failure` text where Core has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecError(pub String);

impl CodecError {
    pub(crate) fn new(what: impl Into<String>) -> CodecError {
        CodecError(what.into())
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodecError {}

// ----------------------------------------------------------------------------------------------
// Encoders.

fn write_i32(v: i32, w: &mut impl CoreWrite) {
    w.write(&v.to_le_bytes());
}

fn write_script(script: &[u8], w: &mut impl CoreWrite) {
    write_compact_size(script.len() as u64, w);
    w.write(script);
}

/// `NetInfoSerWrapper`: the object's own format (Core serializes by dynamic type).
fn write_net_info(net: &NetInfo, w: &mut impl CoreWrite) {
    match net {
        NetInfo::Legacy(NetInfoV1(service)) => {
            w.write(service.addr.as_bytes());
            w.write(&service.port.to_be_bytes());
        }
        NetInfo::Extended(ext) => {
            w.write(&[ext.version]);
            if ext.version != 1 {
                return;
            }
            write_compact_size(ext.entries.len() as u64, w);
            for (purpose, entries) in &ext.entries {
                w.write(&[purpose.to_base()]);
                write_compact_size(entries.len() as u64, w);
                for entry in entries {
                    write_net_entry(entry, w);
                }
            }
        }
    }
}

/// `NetInfoEntry::Serialize`: an address in ADDRv2, a domain, or the invalid tag.
fn write_net_entry(entry: &NIEntry, w: &mut impl CoreWrite) {
    match entry {
        NIEntry::Service(service) => {
            w.write(&[0x01, service.addr.network().to_base()]);
            write_compact_size(service.addr.bytes().len() as u64, w);
            w.write(service.addr.bytes());
            w.write(&service.port.to_be_bytes());
        }
        NIEntry::Domain {
            name,
            port,
        } => {
            w.write(&[0x02]);
            write_compact_size(name.len() as u64, w);
            w.write(name);
            w.write(&port.to_be_bytes());
        }
        NIEntry::Invalid => w.write(&[0xff]),
    }
}

fn write_payouts(payouts: &[PayoutShare], w: &mut impl CoreWrite) {
    write_compact_size(payouts.len() as u64, w);
    for share in payouts {
        write_script(share.script_payout.as_bytes(), w);
        w.write(&share.reward.to_le_bytes());
    }
}

fn write_shares(shares: &[CollateralShare], w: &mut impl CoreWrite) {
    write_compact_size(shares.len() as u64, w);
    for share in shares {
        w.write(&share.amount.to_le_bytes());
        write_script(share.script_refund.as_bytes(), w);
        write_script(share.script_reward.as_bytes(), w);
        w.write(share.key_id_owner.as_bytes());
    }
}

/// `CDeterministicMNState::Serialize` (`dmnstate.h` v24:93-125).
// Core: evo/dmnstate.h:93 CDeterministicMNState::SerializationOp
fn write_state(hot: &MnHot, cold: &MnCold, w: &mut impl CoreWrite) {
    write_i32(cold.version, w);
    write_i32(cold.registered_height, w);
    write_i32(hot.last_paid_height, w);
    write_i32(hot.consecutive_payments, w);
    write_i32(hot.pose_penalty, w);
    write_i32(hot.pose_revived_height, w);
    write_i32(hot.pose_ban_height, w);
    w.write(&cold.revocation_reason.to_le_bytes());
    w.write(cold.confirmed_hash.as_bytes());
    w.write(cold.confirmed_hash_with_pro_tx.as_bytes());
    w.write(cold.key_id_owner.as_bytes());
    w.write(&cold.operator_key_bytes());
    w.write(cold.key_id_voting.as_bytes());
    write_net_info(&cold.net_info, w);
    if cold.version >= EXT_ADDR {
        write_payouts(&cold.payouts, w);
        write_shares(&cold.shares, w);
        w.write(&cold.early_period_blocks.to_le_bytes());
        w.write(&cold.early_penalty.to_le_bytes());
    } else {
        write_script(&cold.script_payout, w);
    }
    write_script(&cold.script_operator_payout, w);
    w.write(cold.platform_node_id.as_bytes());
    if cold.version < EXT_ADDR {
        w.write(&cold.platform_p2p_port.to_le_bytes());
        w.write(&cold.platform_http_port.to_le_bytes());
    }
}

/// The bytes `SerializeHash(*pdmnState)` hashes (`IsEqual` compares states by them).
pub(crate) fn state_bytes(dmn: &Dmn) -> Vec<u8> {
    let mut out = Vec::new();
    write_state(&dmn.hot, &dmn.cold, &mut out);
    out
}

/// `CDeterministicMN::Serialize` at `SER_DISK`/`CLIENT_VERSION` (`deterministicmns.h`
/// v24:65-80): `nType` is always written.
// Core: evo/deterministicmns.h:65 CDeterministicMN::SerializationOp
pub(crate) fn write_dmn(dmn: &Dmn, w: &mut impl CoreWrite) {
    w.write(dmn.pro_tx_hash.as_bytes());
    write_varint(dmn.internal_id, w);
    w.write(dmn.collateral.hash.as_bytes());
    w.write(&dmn.collateral.index.to_le_bytes());
    w.write(&dmn.operator_reward.to_le_bytes());
    write_state(&dmn.hot, &dmn.cold, w);
    w.write(&dmn.mn_type.0.to_le_bytes());
}

/// `CDeterministicMNStateDiff::Serialize` (`dmnstate.h` v24:288-314): `VARINT(fields)`, then
/// each set member in Core's tuple order (which is not the bit order).
// Core: evo/dmnstate.h:288 CDeterministicMNStateDiff::SerializationOp
pub(crate) fn write_state_diff(diff: &MnStateDiff, w: &mut impl CoreWrite) {
    let (hot, cold) = diff.parts();
    let f = |bit: u32| diff.fields & bit != 0;
    write_varint(u64::from(diff.fields), w);
    if f(MnStateDiff::VERSION) {
        write_i32(cold.version, w);
    }
    if f(MnStateDiff::REGISTERED_HEIGHT) {
        write_i32(cold.registered_height, w);
    }
    if f(MnStateDiff::LAST_PAID_HEIGHT) {
        write_i32(hot.last_paid_height, w);
    }
    if f(MnStateDiff::POSE_PENALTY) {
        write_i32(hot.pose_penalty, w);
    }
    if f(MnStateDiff::POSE_REVIVED_HEIGHT) {
        write_i32(hot.pose_revived_height, w);
    }
    if f(MnStateDiff::POSE_BAN_HEIGHT) {
        write_i32(hot.pose_ban_height, w);
    }
    if f(MnStateDiff::REVOCATION_REASON) {
        w.write(&cold.revocation_reason.to_le_bytes());
    }
    if f(MnStateDiff::CONFIRMED_HASH) {
        w.write(cold.confirmed_hash.as_bytes());
    }
    if f(MnStateDiff::CONFIRMED_HASH_WITH_PRO_TX) {
        w.write(cold.confirmed_hash_with_pro_tx.as_bytes());
    }
    if f(MnStateDiff::KEY_ID_OWNER) {
        w.write(cold.key_id_owner.as_bytes());
    }
    if f(MnStateDiff::PUB_KEY_OPERATOR) {
        w.write(&cold.operator_key_bytes());
    }
    if f(MnStateDiff::KEY_ID_VOTING) {
        w.write(cold.key_id_voting.as_bytes());
    }
    if f(MnStateDiff::NET_INFO) {
        write_net_info(&cold.net_info, w);
    }
    if f(MnStateDiff::SCRIPT_PAYOUT) {
        write_script(&cold.script_payout, w);
    }
    if f(MnStateDiff::PAYOUTS) {
        write_payouts(&cold.payouts, w);
    }
    if f(MnStateDiff::SHARES) {
        write_shares(&cold.shares, w);
    }
    if f(MnStateDiff::EARLY_PERIOD_BLOCKS) {
        w.write(&cold.early_period_blocks.to_le_bytes());
    }
    if f(MnStateDiff::EARLY_PENALTY) {
        w.write(&cold.early_penalty.to_le_bytes());
    }
    if f(MnStateDiff::SCRIPT_OPERATOR_PAYOUT) {
        write_script(&cold.script_operator_payout, w);
    }
    if f(MnStateDiff::CONSECUTIVE_PAYMENTS) {
        write_i32(hot.consecutive_payments, w);
    }
    if f(MnStateDiff::PLATFORM_NODE_ID) {
        w.write(cold.platform_node_id.as_bytes());
    }
    if f(MnStateDiff::PLATFORM_P2P_PORT) {
        w.write(&cold.platform_p2p_port.to_le_bytes());
    }
    if f(MnStateDiff::PLATFORM_HTTP_PORT) {
        w.write(&cold.platform_http_port.to_le_bytes());
    }
}

/// `SerializeCanonicalMNList` (`snapshot.h` v24:237-247) for **any** list, the pre-DIP3 default
/// list included: Core's serializer validates nothing.
// Core: evo/snapshot.h:237 SerializeCanonicalMNList
pub fn encode_canonical_list_unvalidated(list: &MnList, w: &mut impl CoreWrite) {
    w.write(list.block_hash().as_bytes());
    write_i32(list.height(), w);
    w.write(&list.total_registered().to_le_bytes());
    write_compact_size(list.len() as u64, w);
    for dmn in list.iter() {
        write_dmn(dmn, w);
    }
}

/// `CanonicalMNListHash(list)` (`snapshot.cpp` v24:150-155): SHA256d of the canonical list
/// bytes, streamed. Defined for every list.
// Core: evo/snapshot.cpp:150 CanonicalMNListHash
pub fn canonical_mn_list_hash(list: &MnList) -> Hash256 {
    let mut hasher = HashWriter::new();
    encode_canonical_list_unvalidated(list, &mut hasher);
    Hash256::from(hasher.finish())
}

/// The storage encoder: what `EvoSnapshot::Validate` requires of its list first
/// (`snapshot.cpp` v24:226-229): `ValidateCanonicalMNInvariants`, then every entry's round trip
/// through the bounded canonical reader (`ValidateMNEncoding`), both in Core's iteration order.
/// Like Core's, the round trip checks each entry's bytes, not that the list reads back as itself.
// Core: evo/snapshot.cpp:104 ValidateCanonicalMNInvariants
pub fn encode_canonical_list(
    list: &MnList,
    net: &dyn NetInfoValidity,
    w: &mut impl CoreWrite,
) -> Result<(), CodecError> {
    if list.height() < 0 {
        return Err(CodecError::new("negative canonical MN-list height"));
    }
    if list.len() > MAX_MNS {
        return Err(CodecError::new("oversized canonical MN list"));
    }
    let order = list.core_order();
    let in_core_order = || order.iter().filter_map(|k| list.get(k));
    let mut prefixes = Vec::with_capacity(list.len());
    let mut max_internal_id = 0;
    for dmn in in_core_order() {
        max_internal_id = max_internal_id.max(dmn.internal_id);
        prefixes.push(dmn.pro_tx_hash.immer_hash());
        check_nested(&dmn.cold, net)
            .map_err(|_| CodecError::new("invalid canonical MN nested collection"))?;
    }
    prefixes.sort_unstable();
    check_prefix_runs(prefixes.into_iter())?;
    if !list.is_empty() && max_internal_id >= u64::from(list.total_registered()) {
        return Err(CodecError::new("canonical MN-list internalId exceeds registration counter"));
    }
    let mut bytes = Vec::new();
    for dmn in in_core_order() {
        bytes.clear();
        write_dmn(dmn, &mut bytes);
        read_canonical_dmn(&mut bytes.as_slice())?;
    }
    encode_canonical_list_unvalidated(list, w);
    Ok(())
}

/// `SerializeCanonicalMNListDiff` (`snapshot.h` v24:304-328): additions by `(internalId,
/// proTxHash)`, updates and removals by ascending id.
// Core: evo/snapshot.h:304 SerializeCanonicalMNListDiff
pub fn encode_canonical_diff(diff: &MnListDiff, w: &mut impl CoreWrite) {
    let mut added: Vec<&Arc<Dmn>> = diff.added.iter().collect();
    added.sort_by_key(|dmn| (dmn.internal_id, dmn.pro_tx_hash));
    write_compact_size(added.len() as u64, w);
    for dmn in added {
        write_dmn(dmn, w);
    }
    let mut updated: Vec<&(u64, MnStateDiff)> = diff.updated.iter().collect();
    updated.sort_by_key(|(id, _)| *id);
    write_compact_size(updated.len() as u64, w);
    for (id, state) in updated {
        write_varint(*id, w);
        write_state_diff(state, w);
    }
    let mut removed = diff.removed.clone();
    removed.sort_unstable();
    removed.dedup();
    write_compact_size(removed.len() as u64, w);
    for id in removed {
        write_varint(id, w);
    }
}

// ----------------------------------------------------------------------------------------------
// Decoders.

fn net_info_error(e: impl fmt::Debug) -> CodecError {
    CodecError::new(format!("net info: {e:?}"))
}

/// A reader over a byte slice. With a budget, every CompactSize it reads counts against it
/// (Core's `SnapshotBoundedInput`).
struct Reader<'a, 'b> {
    input: &'b mut &'a [u8],
    budget: Option<u64>,
}

impl<'a, 'b> Reader<'a, 'b> {
    fn new(input: &'b mut &'a [u8], budget: Option<u64>) -> Self {
        Reader {
            input,
            budget,
        }
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        let (head, rest) = self.input.split_first_chunk::<N>().ok_or_else(eof)?;
        *self.input = rest;
        Ok(*head)
    }

    fn slice(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        if self.input.len() < n {
            return Err(eof());
        }
        let (head, rest) = self.input.split_at(n);
        *self.input = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.bytes::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_le_bytes(self.bytes()?))
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }

    fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(i32::from_le_bytes(self.bytes()?))
    }

    fn i64(&mut self) -> Result<i64, CodecError> {
        Ok(i64::from_le_bytes(self.bytes()?))
    }

    fn varint(&mut self) -> Result<u64, CodecError> {
        read_varint_u64(self.input).map_err(|e| CodecError::new(format!("{e:?}")))
    }

    fn varint_u32(&mut self) -> Result<u32, CodecError> {
        read_varint_u32(self.input).map_err(|e| CodecError::new(format!("{e:?}")))
    }

    /// `ReadCompactSize` (range-checked), charged to the budget.
    fn compact(&mut self) -> Result<u64, CodecError> {
        let n =
            read_compact_size(self.input, true).map_err(|e| CodecError::new(format!("{e:?}")))?;
        self.charge(n)?;
        Ok(n)
    }

    fn charge(&mut self, n: u64) -> Result<(), CodecError> {
        if let Some(budget) = &mut self.budget {
            if n > *budget {
                return Err(CodecError::new("canonical MN nested CompactSize budget exceeded"));
            }
            *budget -= n;
        }
        Ok(())
    }

    fn script(&mut self) -> Result<Vec<u8>, CodecError> {
        let n = self.compact()?;
        Ok(self.slice(usize::try_from(n).map_err(|_| eof())?)?.to_vec())
    }

    /// The CompactSize `at` bytes ahead and the bytes it takes, consuming nothing. `None` where
    /// it does not decode: the base-sdk decode that follows then fails where Core's read does.
    fn peek_compact(&self, at: usize) -> Option<(u64, usize)> {
        let mut rest = self.input.get(at..)?;
        let len = rest.len();
        let n = read_compact_size(&mut rest, true).ok()?;
        Some((n, len - rest.len()))
    }

    /// A base-sdk decode of one item whose CompactSize values the caller has charged.
    fn decode<T: BaseCodec>(&mut self) -> Result<T, CodecError> {
        T::decode(self.input).map_err(|e| CodecError::new(format!("{e:?}")))
    }

    /// `NetInfoSerWrapper`. The extended form is `ExtNetInfo::Unserialize` (`netinfo.h`
    /// rc.4:419-437) with each CompactSize Core's budget sees charged before what it sizes is
    /// read: the group count, each group's entry count, and a domain's name length. A purpose
    /// must be known and above the one before it, or the read fails before its group
    /// ([`netinfo::purpose_follows`], CE4-01). A service's address length is read with
    /// `COMPACTSIZE` (`netaddress.h` v24:435), whose formatter calls the two-argument
    /// `ReadCompactSize` (`serialize.h` v24:720) that `SnapshotBoundedInput`'s overloads do not
    /// replace, so it is not charged. An entry that fails to decode fails the read with its
    /// offset from the start of the network info; the entries after it are never read.
    fn net_info(&mut self, extended: bool) -> Result<NetInfo, CodecError> {
        if !extended {
            let legacy = NetInfoV1::decode(self.input).map_err(net_info_error)?;
            return Ok(NetInfo::Legacy(legacy));
        }
        let start = self.input.len();
        let version = self.u8()?;
        let mut entries = Vec::new();
        if version != 0 && version <= EXT_NET_INFO_VERSION {
            let mut last = None;
            for _ in 0..self.compact()? {
                let purpose = self.u8()?;
                if !netinfo::purpose_follows(purpose, last) {
                    return Err(CodecError::new("ExtNetInfo: purpose unknown or out of order"));
                }
                last = Some(purpose);
                let mut group = Vec::new();
                for _ in 0..self.compact()? {
                    let at = start - self.input.len();
                    // `NetInfoEntry::Unserialize` (v24:243-262): a domain's failures propagate,
                    // so its length is charged after the type byte.
                    if self.input.first().map(|&code| NIEntryCode::from_base(code))
                        == Some(NIEntryCode::Domain)
                        && let Some((n, _)) = self.peek_compact(1)
                    {
                        self.charge(n)?;
                    }
                    let entry = NIEntry::decode(self.input).map_err(|e| {
                        CodecError::new(format!("net info: the entry at byte {at}: {e:?}"))
                    })?;
                    group.push(entry);
                }
                entries.push((NIPurpose::from_base(purpose), group));
            }
        }
        Ok(NetInfo::Extended(NetInfoV2 {
            version,
            entries,
        }))
    }

    fn payouts(&mut self) -> Result<Vec<PayoutShare>, CodecError> {
        let mut payouts = Vec::new();
        for _ in 0..self.compact()? {
            // `scriptPayout` comes first.
            if let Some((n, _)) = self.peek_compact(0) {
                self.charge(n)?;
            }
            payouts.push(self.decode()?);
        }
        Ok(payouts)
    }

    fn shares(&mut self) -> Result<Vec<CollateralShare>, CodecError> {
        let mut shares = Vec::new();
        for _ in 0..self.compact()? {
            // `amount` (8 bytes), then `scriptRefund` and `scriptReward`.
            if let Some((refund, len)) = self.peek_compact(8) {
                self.charge(refund)?;
                let reward_at = usize::try_from(refund).ok().and_then(|n| (8 + len).checked_add(n));
                if let Some((reward, _)) = reward_at.and_then(|at| self.peek_compact(at)) {
                    self.charge(reward)?;
                }
            }
            shares.push(self.decode()?);
        }
        Ok(shares)
    }

    /// The stored operator key: 48 bytes captured under the scheme the version selects.
    fn operator_key(&mut self, version: i32) -> Result<BlsLazyPk, CodecError> {
        let bytes = self.bytes::<48>()?;
        let scheme = scheme_for(version);
        Ok(BlsLazyPk::from_bytes(&bytes, BlsSchemeSource::Version(scheme)))
    }

    /// `CDeterministicMNState::Unserialize`.
    fn state(&mut self) -> Result<(MnHot, MnCold), CodecError> {
        let version = self.i32()?;
        let registered_height = self.i32()?;
        let hot = MnHot {
            last_paid_height: self.i32()?,
            consecutive_payments: self.i32()?,
            pose_penalty: self.i32()?,
            pose_revived_height: self.i32()?,
            pose_ban_height: self.i32()?,
        };
        let revocation_reason = self.u16()?;
        let confirmed_hash = Hash256::from(self.bytes::<32>()?);
        let confirmed_hash_with_pro_tx = Hash256::from(self.bytes::<32>()?);
        let key_id_owner = self.bytes::<20>()?.into();
        let pub_key_operator = self.operator_key(version)?;
        let key_id_voting = self.bytes::<20>()?.into();
        let net_info = Arc::new(self.net_info(version >= EXT_ADDR)?);
        let mut cold = MnCold {
            version,
            registered_height,
            revocation_reason,
            confirmed_hash,
            confirmed_hash_with_pro_tx,
            key_id_owner,
            pub_key_operator,
            key_id_voting,
            net_info,
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
        };
        if version >= EXT_ADDR {
            cold.payouts = self.payouts()?;
            cold.shares = self.shares()?;
            cold.early_period_blocks = self.u32()?;
            cold.early_penalty = self.i64()?;
        } else {
            cold.script_payout = self.script()?;
        }
        cold.script_operator_payout = self.script()?;
        cold.platform_node_id = PlatformNodeId::from(self.bytes::<20>()?);
        if version < EXT_ADDR {
            cold.platform_p2p_port = self.u16()?;
            cold.platform_http_port = self.u16()?;
        }
        Ok((hot, cold))
    }

    /// `CDeterministicMN::Unserialize` at `SER_DISK`.
    fn dmn(&mut self) -> Result<Dmn, CodecError> {
        let pro_tx_hash = ProTxHash::from(self.bytes::<32>()?);
        let internal_id = self.varint()?;
        let collateral = OutPoint {
            hash: TxHash::from(self.bytes::<32>()?),
            index: self.u32()?,
        };
        let operator_reward = self.u16()?;
        let (hot, cold) = self.state()?;
        let mn_type = RawMnType(self.u16()?);
        Ok(Dmn {
            pro_tx_hash,
            internal_id,
            collateral,
            operator_reward,
            mn_type,
            hot,
            cold: Arc::new(cold),
        })
    }

    /// `CDeterministicMNStateDiff::Unserialize`: the set members over Core's default state.
    fn state_diff(&mut self) -> Result<MnStateDiff, CodecError> {
        let fields = self.varint_u32()?;
        let f = |bit: u32| fields & bit != 0;
        if f(MnStateDiff::PUB_KEY_OPERATOR | MnStateDiff::NET_INFO) && !f(MnStateDiff::VERSION) {
            return Err(CodecError::new(
                "Invalid data, nVersion unset when pubKeyOperator or netInfo set",
            ));
        }
        let mut hot = MnHot::default();
        let mut cold = MnCold::core_default();
        if f(MnStateDiff::VERSION) {
            cold.version = self.i32()?;
        }
        if f(MnStateDiff::REGISTERED_HEIGHT) {
            cold.registered_height = self.i32()?;
        }
        if f(MnStateDiff::LAST_PAID_HEIGHT) {
            hot.last_paid_height = self.i32()?;
        }
        if f(MnStateDiff::POSE_PENALTY) {
            hot.pose_penalty = self.i32()?;
        }
        if f(MnStateDiff::POSE_REVIVED_HEIGHT) {
            hot.pose_revived_height = self.i32()?;
        }
        if f(MnStateDiff::POSE_BAN_HEIGHT) {
            hot.pose_ban_height = self.i32()?;
        }
        if f(MnStateDiff::REVOCATION_REASON) {
            cold.revocation_reason = self.u16()?;
        }
        if f(MnStateDiff::CONFIRMED_HASH) {
            cold.confirmed_hash = Hash256::from(self.bytes::<32>()?);
        }
        if f(MnStateDiff::CONFIRMED_HASH_WITH_PRO_TX) {
            cold.confirmed_hash_with_pro_tx = Hash256::from(self.bytes::<32>()?);
        }
        if f(MnStateDiff::KEY_ID_OWNER) {
            cold.key_id_owner = self.bytes::<20>()?.into();
        }
        if f(MnStateDiff::PUB_KEY_OPERATOR) {
            cold.pub_key_operator = self.operator_key(cold.version)?;
        }
        if f(MnStateDiff::KEY_ID_VOTING) {
            cold.key_id_voting = self.bytes::<20>()?.into();
        }
        if f(MnStateDiff::NET_INFO) {
            cold.net_info = Arc::new(self.net_info(cold.version >= EXT_ADDR)?);
        }
        if f(MnStateDiff::SCRIPT_PAYOUT) {
            cold.script_payout = self.script()?;
        }
        if f(MnStateDiff::PAYOUTS) {
            cold.payouts = self.payouts()?;
        }
        if f(MnStateDiff::SHARES) {
            cold.shares = self.shares()?;
        }
        if f(MnStateDiff::EARLY_PERIOD_BLOCKS) {
            cold.early_period_blocks = self.u32()?;
        }
        if f(MnStateDiff::EARLY_PENALTY) {
            cold.early_penalty = self.i64()?;
        }
        if f(MnStateDiff::SCRIPT_OPERATOR_PAYOUT) {
            cold.script_operator_payout = self.script()?;
        }
        if f(MnStateDiff::CONSECUTIVE_PAYMENTS) {
            hot.consecutive_payments = self.i32()?;
        }
        if f(MnStateDiff::PLATFORM_NODE_ID) {
            cold.platform_node_id = PlatformNodeId::from(self.bytes::<20>()?);
        }
        if f(MnStateDiff::PLATFORM_P2P_PORT) {
            cold.platform_p2p_port = self.u16()?;
        }
        if f(MnStateDiff::PLATFORM_HTTP_PORT) {
            cold.platform_http_port = self.u16()?;
        }
        Ok(MnStateDiff::from_parts(fields, hot, Arc::new(cold)))
    }
}

fn eof() -> CodecError {
    CodecError::new("end of data")
}

/// `MaterializeLazyBLSFields`: the stored key after Core's `Get()`, so an undecodable key reads
/// back as the default key (all zeros) and can never be canonical unless its bytes were zeros.
fn materialized(cold: &mut MnCold) {
    let key = cold.pub_key_operator.clone();
    let _ = key.get();
    cold.pub_key_operator = key;
}

/// The canonical reader's nested-collection rules shared with the encoder's invariants.
fn check_nested(cold: &MnCold, net: &dyn NetInfoValidity) -> Result<(), CodecError> {
    if cold.payouts.len() > MAX_PAYOUT_SHARES {
        return Err(CodecError::new("oversized canonical MN payout list"));
    }
    if !cold.net_info.is_empty() && !net.is_valid(&cold.net_info) {
        return Err(CodecError::new("invalid canonical MN network info"));
    }
    Ok(())
}

/// Counts `prefix` into the run of equal leading-64-bit hash prefixes that `prev` (the previous
/// entry's prefix) ends, and refuses a run past [`MAX_HASH_PREFIX_RUN`].
fn extend_prefix_run(run: &mut usize, prev: Option<u64>, prefix: u64) -> Result<(), CodecError> {
    *run = if prev == Some(prefix) {
        *run + 1
    } else {
        1
    };
    if *run > MAX_HASH_PREFIX_RUN {
        return Err(CodecError::new("canonical MN-list hash-prefix run exceeds collision bound"));
    }
    Ok(())
}

fn check_prefix_runs(sorted_prefixes: impl Iterator<Item = u64>) -> Result<(), CodecError> {
    let mut run = 0usize;
    let mut previous = None;
    for prefix in sorted_prefixes {
        extend_prefix_run(&mut run, previous, prefix)?;
        previous = Some(prefix);
    }
    Ok(())
}

/// `CheckCanonicalEncoding`: what `write` re-encodes must equal the bytes consumed between
/// `start` and `rest` (the input before and after the decode).
fn check_canonical(
    start: &[u8],
    rest: &[u8],
    write: impl FnOnce(&mut Vec<u8>),
) -> Result<(), CodecError> {
    let consumed = &start[..start.len() - rest.len()];
    let mut again = Vec::with_capacity(consumed.len());
    write(&mut again);
    if again != consumed {
        return Err(CodecError::new("noncanonical MN object encoding"));
    }
    Ok(())
}

/// One canonical entry: decoded under the per-entry budget, the lazy key materialized, then
/// re-encoded and compared with the consumed bytes (`CheckCanonicalEncoding`), with its internal
/// id present.
fn read_canonical_dmn(input: &mut &[u8]) -> Result<Dmn, CodecError> {
    let start = *input;
    let mut dmn = Reader::new(input, Some(MAX_MN_COMPACT_ITEMS)).dmn()?;
    materialized(Arc::make_mut(&mut dmn.cold));
    check_canonical(start, input, |again| write_dmn(&dmn, again))?;
    if dmn.internal_id == u64::MAX {
        return Err(CodecError::new("invalid canonical MN internalId"));
    }
    Ok(dmn)
}

/// `UnserializeCanonicalMNList` (`snapshot.h` v24:249-298), with Core's rules and texts. The
/// list is [`crate::Provenance::Decoded`].
// Core: evo/snapshot.h:249 UnserializeCanonicalMNList
pub fn decode_canonical_list(
    input: &mut &[u8],
    net: &dyn NetInfoValidity,
) -> Result<MnList, CodecError> {
    let mut r = Reader::new(input, None);
    let block_hash = BlockHash::from(r.bytes::<32>()?);
    let height = r.i32()?;
    let total_registered = r.u32()?;
    if height < 0 {
        return Err(CodecError::new("negative canonical MN-list height"));
    }
    let mut list = MnList::new(block_hash, height, total_registered);
    let count = bounded_count(&mut r, "MN count")?;
    let mut previous: Option<ProTxHash> = None;
    let mut max_internal_id = 0;
    let mut prefix_run = 0;
    for _ in 0..count {
        let dmn = read_canonical_dmn(r.input)?;
        check_nested(&dmn.cold, net)?;
        if previous.is_some_and(|p| p >= dmn.pro_tx_hash) {
            return Err(CodecError::new("noncanonical canonical MN-list order"));
        }
        extend_prefix_run(
            &mut prefix_run,
            previous.map(|p| p.immer_hash()),
            dmn.pro_tx_hash.immer_hash(),
        )?;
        previous = Some(dmn.pro_tx_hash);
        max_internal_id = max_internal_id.max(dmn.internal_id);
        list.add_mn(Arc::new(dmn), false)
            .map_err(|e| CodecError::new(format!("invalid canonical MN list: {e}")))?;
    }
    if count != 0 && max_internal_id >= u64::from(total_registered) {
        return Err(CodecError::new("canonical MN-list internalId exceeds registration counter"));
    }
    Ok(list)
}

fn bounded_count(r: &mut Reader<'_, '_>, field: &str) -> Result<usize, CodecError> {
    let n = r.compact()?;
    if n > MAX_MNS as u64 {
        return Err(CodecError::new(format!("oversized evo snapshot {field}")));
    }
    Ok(n as usize)
}

/// `UnserializeCanonicalMNListDiff(s, remaining_operations)` (`snapshot.h` v24:331-387).
// Core: evo/snapshot.h:331 UnserializeCanonicalMNListDiff
pub fn decode_canonical_diff(
    input: &mut &[u8],
    remaining_operations: &mut usize,
    net: &dyn NetInfoValidity,
) -> Result<MnListDiff, CodecError> {
    let mut diff = MnListDiff::default();
    let spend = |n: usize, budget: &mut usize| {
        if n > *budget {
            return Err(CodecError::new("historical MN-diff operation budget exceeded"));
        }
        *budget -= n;
        Ok(())
    };
    let added = bounded_count(&mut Reader::new(input, None), "MN-diff additions")?;
    spend(added, remaining_operations)?;
    let mut previous_id = None;
    for _ in 0..added {
        let dmn = read_canonical_dmn(input)?;
        if previous_id.is_some_and(|p| p >= dmn.internal_id)
            || check_nested(&dmn.cold, net).is_err()
        {
            return Err(CodecError::new("noncanonical canonical MN-diff addition"));
        }
        previous_id = Some(dmn.internal_id);
        diff.added.push(Arc::new(dmn));
    }
    let updated = bounded_count(&mut Reader::new(input, None), "MN-diff updates")?;
    spend(updated, remaining_operations)?;
    let mut previous_id = None;
    for _ in 0..updated {
        let id = Reader::new(input, None).varint()?;
        if previous_id.is_some_and(|p| p >= id) {
            return Err(CodecError::new("noncanonical canonical MN-diff update order"));
        }
        let start = *input;
        let state = Reader::new(input, Some(MAX_MN_COMPACT_ITEMS)).state_diff()?;
        let state = if state.fields & MnStateDiff::PUB_KEY_OPERATOR != 0 {
            let (hot, cold) = state.parts();
            let mut cold = cold.clone();
            materialized(&mut cold);
            MnStateDiff::from_parts(state.fields, *hot, Arc::new(cold))
        } else {
            state
        };
        check_canonical(start, input, |again| write_state_diff(&state, again))?;
        diff.updated.push((id, state));
        previous_id = Some(id);
    }
    let removed = bounded_count(&mut Reader::new(input, None), "MN-diff removals")?;
    spend(removed, remaining_operations)?;
    let mut previous_id = None;
    for _ in 0..removed {
        let id = Reader::new(input, None).varint()?;
        if previous_id.is_some_and(|p| p >= id) {
            return Err(CodecError::new("noncanonical canonical MN-diff removal order"));
        }
        diff.removed.push(id);
        previous_id = Some(id);
    }
    Ok(diff)
}

// ----------------------------------------------------------------------------------------------
// Our records (EV2 §2.9 envelope): `u8 record_version ‖ u8 codec_id ‖ VARINT height ‖ body ‖
// trailer`.

/// The envelope version.
const RECORD_VERSION: u8 = 1;
/// A canonical list body with a collision-bucket trailer (`mn_snapshot`).
const CODEC_CANONICAL_LIST: u8 = 1;
/// A canonical diff body with a counter and collision-bucket trailer (`mn_diff`).
const CODEC_CANONICAL_DIFF: u8 = 2;

/// Stored DML bytes: only this module mints them, so no stored DML byte comes from another
/// encoder (EV2 §2.9, the bytes-as-a-type rule).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DmlRecordBytes(Box<[u8]>);

impl DmlRecordBytes {
    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A decoded record.
#[derive(Clone, Debug)]
pub enum DmlRecord {
    /// An `mn_snapshot` row.
    Snapshot(Arc<MnList>),
    /// An `mn_diff` row.
    Diff(MnListDiffRecord),
}

fn write_buckets<'a>(
    buckets: impl ExactSizeIterator<Item = (u64, &'a [ProTxHash])>,
    w: &mut impl CoreWrite,
) {
    write_compact_size(buckets.len() as u64, w);
    for (hash, members) in buckets {
        w.write(&hash.to_le_bytes());
        write_compact_size(members.len() as u64, w);
        for member in members {
            w.write(member.as_bytes());
        }
    }
}

fn read_buckets(input: &mut &[u8]) -> Result<Vec<(u64, Vec<ProTxHash>)>, CodecError> {
    let mut r = Reader::new(input, None);
    let n = r.compact()?;
    let mut buckets: Vec<(u64, Vec<ProTxHash>)> = Vec::new();
    for _ in 0..n {
        let hash = u64::from_le_bytes(r.bytes()?);
        if buckets.last().is_some_and(|(h, _)| *h >= hash) {
            return Err(CodecError::new("collision trailer out of order"));
        }
        let m = r.compact()?;
        if m == 1 {
            return Err(CodecError::new("collision trailer bucket with one member"));
        }
        if m > MAX_HASH_PREFIX_RUN as u64 {
            return Err(CodecError::new("collision trailer bucket exceeds collision bound"));
        }
        let mut members = Vec::new();
        for _ in 0..m {
            let member = ProTxHash::from(r.bytes::<32>()?);
            if member.immer_hash() != hash || members.contains(&member) {
                return Err(CodecError::new(
                    "collision trailer member does not belong to its bucket",
                ));
            }
            members.push(member);
        }
        buckets.push((hash, members));
    }
    Ok(buckets)
}

fn envelope(codec_id: u8, height: i32) -> Result<Vec<u8>, CodecError> {
    let height = u64::try_from(height).map_err(|_| CodecError::new("negative record height"))?;
    let mut out = vec![RECORD_VERSION, codec_id];
    write_varint(height, &mut out);
    Ok(out)
}

/// The `mn_snapshot` value of `list`: the validated canonical list and every collision bucket.
pub fn encode_snapshot_record(
    list: &MnList,
    net: &dyn NetInfoValidity,
) -> Result<DmlRecordBytes, CodecError> {
    let mut out = envelope(CODEC_CANONICAL_LIST, list.height())?;
    encode_canonical_list(list, net, &mut out)?;
    let buckets: Vec<(u64, Vec<ProTxHash>)> =
        list.collision_buckets().map(|(h, b)| (h, b.iter().copied().collect())).collect();
    write_buckets(buckets.iter().map(|(h, m)| (*h, m.as_slice())), &mut out);
    Ok(DmlRecordBytes(out.into_boxed_slice()))
}

/// The `mn_diff` value of `record`: the canonical diff, the counter after the block, and the
/// collision trailer. The diff is read back with the canonical reader, so a stored diff is
/// always readable: an addition the reader refuses fails here, at write time, instead of every
/// later `resolve_list` across the block.
pub fn encode_diff_record(
    record: &MnListDiffRecord,
    net: &dyn NetInfoValidity,
) -> Result<DmlRecordBytes, CodecError> {
    let mut out = envelope(CODEC_CANONICAL_DIFF, record.height)?;
    let start = out.len();
    encode_canonical_diff(&record.diff, &mut out);
    decode_canonical_diff(&mut &out[start..], &mut { usize::MAX }, net)?;
    out.extend_from_slice(&record.total_registered_after.to_le_bytes());
    write_buckets(record.collision_buckets_after.iter().map(|(h, m)| (*h, m.as_slice())), &mut out);
    Ok(DmlRecordBytes(out.into_boxed_slice()))
}

/// Decodes a stored record: the envelope, the body with Core's canonical reader, and the
/// trailer, which must consume the rest. A snapshot's buckets are restored and the list's
/// invariants checked.
pub fn decode_record(bytes: &[u8], net: &dyn NetInfoValidity) -> Result<DmlRecord, CodecError> {
    let mut input = bytes;
    let mut r = Reader::new(&mut input, None);
    let version = r.u8()?;
    if version != RECORD_VERSION {
        return Err(CodecError::new(format!("unknown DML record version {version}")));
    }
    let codec_id = r.u8()?;
    let height =
        i32::try_from(r.varint()?).map_err(|_| CodecError::new("record height out of range"))?;
    let record = match codec_id {
        CODEC_CANONICAL_LIST => {
            let mut list = decode_canonical_list(&mut input, net)?;
            if list.height() != height {
                return Err(CodecError::new("snapshot height differs from its envelope"));
            }
            // A snapshot's trailer is canonical: exactly the list's buckets, as the encoder
            // writes them.
            let buckets = read_buckets(&mut input)?;
            if buckets.iter().any(|(_, members)| members.is_empty()) {
                return Err(CodecError::new("snapshot collision trailer bucket with no member"));
            }
            list.restore_buckets(buckets.iter().map(|(h, m)| (*h, m.as_slice())))
                .map_err(|e| CodecError::new(e.to_string()))?;
            if !list.collision_buckets_map().keys().eq(buckets.iter().map(|(h, _)| h)) {
                return Err(CodecError::new("snapshot collision trailer misses a bucket"));
            }
            list.check_invariants().map_err(|e| CodecError::new(e.to_string()))?;
            DmlRecord::Snapshot(Arc::new(list))
        }
        CODEC_CANONICAL_DIFF => {
            let mut budget = usize::MAX;
            let diff = decode_canonical_diff(&mut input, &mut budget, net)?;
            let total_registered_after = Reader::new(&mut input, None).u32()?;
            let collision_buckets_after = read_buckets(&mut input)?;
            DmlRecord::Diff(MnListDiffRecord {
                height,
                total_registered_after,
                diff,
                collision_buckets_after,
            })
        }
        other => {
            return Err(CodecError::new(format!(
                "unknown DML codec id {other} (record version {version})"
            )));
        }
    };
    if !input.is_empty() {
        return Err(CodecError::new("trailing bytes after a DML record"));
    }
    Ok(record)
}

/// Core's evodb encodings, decode-only, for EV4's importer of Core datadirs (EV2 §2.9).
///
/// Unlike the canonical readers, these leave operator keys undecoded, as Core's evodb reader
/// does. A key that fails to decode later would leave the list's unique-property slots out of
/// step with Core's, which treats such a key as the default one from then on. Core never writes
/// an undecodable key on a valid chain. Before an imported list is used it needs two separate
/// checks (EV2a D16):
/// - its [`canonical_mn_list_hash`] equal to a trusted expected hash, which shows it is the
///   intended list in canonical content (not in collision-bucket order, which a reload
///   reverses; quirk 41). Hashing writes each key's captured bytes and decodes none.
/// - [`encode_canonical_list`] succeeding (Core's `EvoSnapshot::Validate`), which reads every
///   entry back through the canonical reader. That reader decodes each operator key as Core's
///   `MaterializeLazyBLSFields` does, so a key that does not decode is refused as noncanonical.
pub mod core_disk {
    use super::*;

    /// `CDeterministicMNList::Unserialize` (`deterministicmns.h` v24:246-256): the entries in the
    /// file's (CHAMP) order, each through `AddMN(dmn, false)`, so collision buckets end up as
    /// Core's reloaded map holds them.
    // Core: evo/deterministicmns.h:246 CDeterministicMNList::Unserialize
    pub fn decode_list_s3(input: &mut &[u8]) -> Result<MnList, CodecError> {
        let (block_hash, height, total_registered, n) = read_list_head(input)?;
        let mut list = MnList::new(block_hash, height, total_registered);
        for _ in 0..n {
            let dmn = Reader::new(input, None).dmn()?;
            list.add_mn(Arc::new(dmn), false).map_err(|e| CodecError::new(e.to_string()))?;
        }
        Ok(list)
    }

    /// The list's block hash, height, `nTotalRegisteredCount` and entry count; the entries
    /// follow (the reference twin's reader adds them itself).
    pub(crate) fn read_list_head(
        input: &mut &[u8],
    ) -> Result<(BlockHash, i32, u32, u64), CodecError> {
        let mut r = Reader::new(input, None);
        Ok((BlockHash::from(r.bytes::<32>()?), r.i32()?, r.u32()?, r.compact()?))
    }

    /// One `CDeterministicMN` as evodb stores it: what `GetAllQuorumMembers` returns for each
    /// member, in the oracle's contexts.
    // Core: evo/deterministicmns.h CDeterministicMN::Unserialize
    pub fn decode_dmn(input: &mut &[u8]) -> Result<Dmn, CodecError> {
        Reader::new(input, None).dmn()
    }

    /// `CDeterministicMNListDiff::Unserialize` (new format, `deterministicmns.h` v24:676-700):
    /// updates in any order (v23 wrote `unordered_map` order); a repeated id keeps the first, a
    /// repeated removal is one, as Core's containers do. Updates and removals come out sorted
    /// by id as `BuildDiff`'s; additions stay in file order, as Core keeps them.
    // Core: evo/deterministicmns.h:676 CDeterministicMNListDiff::UnserializeImpl
    pub fn decode_diff_d4(input: &mut &[u8]) -> Result<MnListDiff, CodecError> {
        let mut diff = MnListDiff::default();
        let n = Reader::new(input, None).compact()?;
        for _ in 0..n {
            diff.added.push(Arc::new(Reader::new(input, None).dmn()?));
        }
        let n = Reader::new(input, None).compact()?;
        for _ in 0..n {
            let id = Reader::new(input, None).varint()?;
            let state = Reader::new(input, None).state_diff()?;
            diff.updated.push((id, state));
        }
        let n = Reader::new(input, None).compact()?;
        for _ in 0..n {
            diff.removed.push(Reader::new(input, None).varint()?);
        }
        // A stable sort keeps a repeated id's entries in file order: the first stays, as Core's
        // `emplace` keeps it.
        diff.updated.sort_by_key(|(id, _)| *id);
        diff.updated.dedup_by_key(|(id, _)| *id);
        diff.removed.sort_unstable();
        diff.removed.dedup();
        Ok(diff)
    }
}

#[cfg(test)]
mod tests;
