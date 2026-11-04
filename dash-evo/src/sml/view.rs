//! The SML as LQ1's list view (SB0 §5.1; LQ1 §2.2.4): SPV's input to a commitment's
//! `membersSig` check.
//!
//! Every field LQ1's non-rotated selection reads is in the entry: proTxHash, the confirmed hash
//! (whose stored hash with the proTxHash is a function of it), validity (`isValid` is
//! `!IsBanned()`) and the type. Two are not, and the view says so:
//! - the collateral outpoint, the tie-break of equal scores, is the null outpoint (B-n3): equal
//!   scores need a SHA256 collision (LQ1 §3.4);
//! - `try_add` ([`SmlScratch`]) checks what the entry carries (proTxHash, the network entries,
//!   the operator key, an Evo node's platform id) and not the collateral, the internal id or the
//!   owner key, so on canonical on-chain SML entries it can only accept where Core's `AddMN`
//!   rejects (under-reject; `UniqueKey` gives the one non-canonical exception). It must never
//!   back a node or consensus path.
//!
//! The entry's operator key is the one `membersSig` is checked against
//! ([`OperatorKeyView`]).

use std::collections::BTreeMap;

use dash_consensus::hash::sha256;
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::BlsPkValue;
use dash_primitives::{NIEntry, NetInfo, NetInfoV1, OutPoint, ServiceV2, TxHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;

use super::entry::SmlEntry;
use super::root::SmlList;
use crate::llmq::{
    AddMnRejected, ListCounts, MnEntryView, MnListView, OperatorKeyView, ScratchMnList,
};
use crate::types::ProTxHash;

/// The SML entry version whose operator key is stored in the legacy scheme
/// (`ProTxVersion::LegacyBLS`).
const SML_VERSION_LEGACY_BLS: u16 = 1;

/// One SML entry as LQ1 reads it: the entry and the values Core's masternode state stores
/// beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmlMember {
    entry: SmlEntry,
    /// `confirmedHashWithProRegTxHash`: single SHA256(proTxHash ‖ confirmedHash) once confirmed
    /// (`evo/dmnstate.h` v24:162-169); zero while unconfirmed, as the state Core never updated.
    confirmed_hash_with_protx: [u8; 32],
    /// The null `COutPoint` (B-n3): the SML carries no collateral.
    collateral: OutPoint,
}

impl SmlMember {
    /// The member of `entry`.
    pub fn new(entry: &SmlEntry) -> SmlMember {
        let confirmed = entry.confirmed_hash.as_bytes();
        let confirmed_hash_with_protx = if confirmed == &[0u8; 32] {
            [0u8; 32]
        } else {
            let mut pre = [0u8; 64];
            pre[..32].copy_from_slice(entry.pro_reg_tx_hash.as_bytes());
            pre[32..].copy_from_slice(confirmed);
            sha256(&pre)
        };
        SmlMember {
            entry: entry.clone(),
            confirmed_hash_with_protx,
            collateral: OutPoint {
                hash: TxHash::from([0u8; 32]),
                index: u32::MAX,
            },
        }
    }

    /// The SML entry.
    pub fn entry(&self) -> &SmlEntry {
        &self.entry
    }

    /// The scheme the entry's operator key is written in: legacy iff the entry's version is 1
    /// (`CBLSLazyPublicKeyVersionWrapper`, `evo/simplifiedmns.h` v24:83).
    fn key_scheme(&self) -> BlsSchemeKind {
        BlsSchemeKind::from_legacy(self.entry.version == SML_VERSION_LEGACY_BLS)
    }
}

impl MnEntryView for SmlMember {
    fn pro_tx_hash(&self) -> &ProTxHash {
        &self.entry.pro_reg_tx_hash
    }
    fn collateral(&self) -> &OutPoint {
        &self.collateral
    }
    fn is_evo(&self) -> bool {
        self.entry.mn_type.is_evo()
    }
    fn is_banned(&self) -> bool {
        !self.entry.is_valid
    }
    fn confirmed_hash_is_null(&self) -> bool {
        self.entry.confirmed_hash.as_bytes() == &[0u8; 32]
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        &self.confirmed_hash_with_protx
    }
}

impl OperatorKeyView for SmlMember {
    /// The masternode state's lazy key, `Get()`: the bytes decoded under the entry version's
    /// scheme with no fallback, the invalid object when they do not decode.
    fn operator_key(&self) -> BlsPkValue {
        BlsPkValue::from_canonical(&self.entry.operator_key, self.key_scheme())
            .unwrap_or_else(BlsPkValue::invalid)
    }
}

impl MnListView for SmlList {
    type Entry = SmlMember;
    type Scratch = SmlScratch;

    /// Total: every entry. Enabled: the valid ones (`isValid` is `!IsBanned()`).
    fn counts(&self) -> ListCounts {
        ListCounts {
            total: self.len(),
            enabled: self.entries().iter().filter(|e| e.is_valid).count(),
        }
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.get(p).is_some()
    }
    fn is_pose_banned(&self, p: &ProTxHash) -> bool {
        self.get(p).is_some_and(|e| !e.is_valid)
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&SmlMember)) {
        for entry in self.entries().iter().filter(|e| !only_valid || e.is_valid) {
            f(&SmlMember::new(entry));
        }
    }
    fn new_scratch(&self) -> SmlScratch {
        SmlScratch::default()
    }
}

/// A unique property `AddMN` registers that the SML carries, keyed by its value. Equal keys are
/// equal values, which Core's serialization hash also finds equal.
///
/// The converse holds for canonical on-chain SML entries only. Core's `GetUniquePropertyHash` is
/// an untagged `SerializeHash`, so a native IPv6 service whose 18-byte ADDRv1 form equals a
/// domain entry's serialization (address `0x0f ‖ "aaa.example.com"`, port 9999, against
/// `DomainPort("aaa.example.com", 9999)`) is one hash in Core and two keys here (SB8 D5). A
/// canonical DML or SML does not carry that pair, so the keys are not merged.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum UniqueKey {
    /// A `CService`, from a legacy or an extended entry alike: its network, address bytes and
    /// port. Core hashes one as `CService` either way (`GetUniquePropertyHash`, ADDRv2 only for
    /// an address ADDRv1 cannot carry, `evo/deterministicmns.h` v24:526-545), so a service
    /// is one value whichever entry form holds it.
    Service(u8, Vec<u8>, u16),
    /// An extended domain entry: the name and port.
    Domain(Vec<u8>, u16),
    OperatorKey([u8; 48]),
    PlatformNodeId([u8; 20]),
}

impl UniqueKey {
    fn service(service: &ServiceV2) -> UniqueKey {
        UniqueKey::Service(
            service.addr.network().to_base(),
            service.addr.bytes().to_vec(),
            service.port,
        )
    }
}

/// Whether Core keeps a legacy service: `NetInfoEntry{service}` holds it only when
/// `service.IsValid()` (`evo/netinfo.h` v24:212-217), else `GetEntries` gives nothing
/// (`evo/netinfo.cpp` v24:331-339). base-sdk's `NetInfoV1` decoder is Core's `CNetAddr::IsValid`
/// on the legacy form (an address it does not keep reads as the empty service), so this decodes
/// the value's wire form (the 16-byte address, then the port big-endian) with it. Decoded SML
/// entries are already normalised so; this covers a hand-built one.
fn legacy_service_kept(v1: &NetInfoV1) -> bool {
    let mut wire = [0u8; 18];
    wire[..16].copy_from_slice(&v1.0.addr.0);
    wire[16..].copy_from_slice(&v1.0.port.to_be_bytes());
    NetInfoV1::decode(&mut &wire[..]).is_ok_and(|kept| !kept.0.addr.is_null())
}

/// A rotation's `MnsUsedAtH` set over SML entries: Core's `AddMN` restricted to what the SML
/// carries (LQ1 §2.2.4). It rejects a duplicate proTxHash, a network entry Core cannot read, and a
/// network entry, non-null operator key or (Evo) non-null platform node id another masternode
/// holds, all or nothing. It does not check the collateral, the internal id or the owner key.
///
/// It therefore deliberately breaks [`ScratchMnList`]'s contract that the exact acceptance rule
/// is consensus: an [`SmlList`] serves SPV's `membersSig` input only (a divergence can only leave
/// a quorum unverified, SB0 §5.1) and must never back a node or consensus path.
#[derive(Clone, Debug, Default)]
pub struct SmlScratch {
    members: BTreeMap<ProTxHash, SmlMember>,
    unique: BTreeMap<UniqueKey, ProTxHash>,
}

impl SmlScratch {
    /// The properties `AddMN` registers for `e`, in its order; `None` for an entry Core throws
    /// on ("invalid address").
    // Core: evo/deterministicmns.cpp:439 CDeterministicMNList::AddMN
    fn properties(e: &SmlEntry) -> Option<Vec<UniqueKey>> {
        let mut keys = Vec::new();
        match &e.net_info {
            NetInfo::Legacy(v1) => {
                if legacy_service_kept(v1) {
                    keys.push(UniqueKey::service(&ServiceV2::from(v1.0.clone())));
                }
            }
            NetInfo::Extended(v2) => {
                for entry in v2.entries.iter().flat_map(|(_, group)| group) {
                    keys.push(match entry {
                        NIEntry::Service(service) => UniqueKey::service(service),
                        NIEntry::Domain {
                            name,
                            port,
                        } => UniqueKey::Domain(name.clone(), *port),
                        NIEntry::Invalid => return None,
                    });
                }
            }
        }
        if e.operator_key != [0u8; 48] {
            keys.push(UniqueKey::OperatorKey(e.operator_key));
        }
        if e.mn_type.is_evo() && e.platform_node_id.as_bytes() != &[0u8; 20] {
            keys.push(UniqueKey::PlatformNodeId(*e.platform_node_id.as_bytes()));
        }
        Some(keys)
    }
}

impl ScratchMnList for SmlScratch {
    type Entry = SmlMember;

    fn try_add(&mut self, e: &SmlMember) -> Result<(), AddMnRejected> {
        let p = e.entry.pro_reg_tx_hash;
        if self.members.contains_key(&p) {
            return Err(AddMnRejected);
        }
        let keys = Self::properties(&e.entry).ok_or(AddMnRejected)?;
        // `AddUniqueProperty`: a value another masternode holds fails; the same one counts again.
        if keys.iter().any(|k| self.unique.get(k).is_some_and(|owner| *owner != p)) {
            return Err(AddMnRejected);
        }
        self.unique.extend(keys.into_iter().map(|k| (k, p)));
        self.members.insert(p, e.clone());
        Ok(())
    }
    fn contains(&self, p: &ProTxHash) -> bool {
        self.members.contains_key(p)
    }
    fn total(&self) -> usize {
        self.members.len()
    }
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&SmlMember)) {
        for member in self.members.values().filter(|m| !only_valid || m.entry.is_valid) {
            f(member);
        }
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
