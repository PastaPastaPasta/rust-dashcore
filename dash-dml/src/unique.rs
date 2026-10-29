//! Core's unique-property map (EV2 §2.2; quirk 23), keyed by the serialization pre-image.

use std::sync::Arc;

use dash_consensus::primitives::write_compact_size;
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{AddrV2, NIEntry, NetInfo, OutPoint, PlatformNodeId, ServiceV1, ServiceV2};
use dash_types::Numeric;
use imbl::shared_ptr::DefaultSharedPtr;

use crate::ProTxHash;
use crate::types::{KeyId, MnCold, OperatorKey, OperatorPoint};

/// Core's `GetUniquePropertyHash` *pre-image*: the `SER_GETHASH` serialization of the value at
/// `PROTOCOL_VERSION` (+ `ADDRV2_FORMAT` for a `CService` that is not AddrV1-compatible).
///
/// Two values share a slot in Core iff their serializations are equal (SHA256d of equal bytes),
/// so keying by the bytes keeps every collision class of quirk 23 (a `CKeyID` and a `uint160`
/// with equal bytes, a CJDNS service and a key id, a TORv3 service and an outpoint, the legacy
/// and basic encodings of one BLS point) without a SHA256d per lookup.
///
/// `Ord` is the bytes' lexicographic order, for ordered containers (PF0 measures an `OrdMap`
/// against the hash table, EV2 §2.2); Core never orders these keys.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UniqueKey(Box<[u8]>);

impl UniqueKey {
    /// A collateral outpoint: `txid[32] ‖ u32 n` (36 bytes, `COutPoint`'s serialization).
    pub fn collateral(op: &OutPoint) -> Self {
        let mut bytes = Vec::with_capacity(36);
        bytes.extend_from_slice(op.hash.as_bytes());
        bytes.extend_from_slice(&op.index.to_le_bytes());
        UniqueKey(bytes.into_boxed_slice())
    }

    /// An owner key id (or a share's owner key id): the 20 bytes of the `CKeyID`.
    pub fn key_id(k: &KeyId) -> Self {
        UniqueKey(Box::from(&k.as_bytes()[..]))
    }

    /// A `platformNodeID` (`uint160`): its 20 bytes, the same slot as a key id with equal bytes
    /// (quirk 23).
    pub fn platform_node_id(p: &PlatformNodeId) -> Self {
        UniqueKey(Box::from(&p.as_bytes()[..]))
    }

    /// A legacy (`MnNetInfo`) service: `CService` in the ADDRv1 format, `ip[16] ‖ port` big
    /// endian (18 bytes). Every address a legacy service holds is AddrV1-compatible.
    pub fn service_v1(s: &ServiceV1) -> Self {
        let mut bytes = Vec::with_capacity(18);
        bytes.extend_from_slice(s.addr.as_bytes());
        bytes.extend_from_slice(&s.port.to_be_bytes());
        UniqueKey(bytes.into_boxed_slice())
    }

    /// A service entry: `CService` in the ADDRv1 format (IPv4 mapped into IPv6, or IPv6:
    /// `ip[16] ‖ port`, 18 bytes) when it is AddrV1-compatible, else in the ADDRv2 format
    /// (`network id ‖ CompactSize(len) ‖ addr ‖ port`), the port big endian
    /// (`deterministicmns.h` v24:537-544).
    // Core: evo/deterministicmns.h:526 CDeterministicMNList::GetUniquePropertyHash
    pub fn service(s: &ServiceV2) -> Self {
        let mut bytes = Vec::with_capacity(38);
        match &s.addr {
            AddrV2::Ipv4(ip) => {
                bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]);
                bytes.extend_from_slice(ip);
            }
            AddrV2::Ipv6(ip) => bytes.extend_from_slice(ip),
            addr => {
                bytes.push(addr.network().to_base());
                write_compact_size(addr.bytes().len() as u64, &mut bytes);
                bytes.extend_from_slice(addr.bytes());
            }
        }
        bytes.extend_from_slice(&s.port.to_be_bytes());
        UniqueKey(bytes.into_boxed_slice())
    }

    /// A domain entry: `DomainPort`'s serialization, `LIMITED_STRING(addr, 253) ‖ port` big
    /// endian (`netinfo.h` v24:176-180).
    pub fn domain(name: &[u8], port: u16) -> Self {
        let mut bytes = Vec::with_capacity(name.len() + 3);
        write_compact_size(name.len() as u64, &mut bytes);
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(&port.to_be_bytes());
        UniqueKey(bytes.into_boxed_slice())
    }

    /// An operator key: `CBLSLazyPublicKey`'s plain serialization, the 48 bytes under the scheme
    /// the key captured (zeros for the default key). Serialized from a clone, so the key's own
    /// state is never touched.
    pub fn operator_key(k: &OperatorKey) -> Self {
        UniqueKey(Box::from(&k.clone().to_bytes()[..]))
    }

    /// A decoded key set under `scheme` (Core's `CBLSLazyPublicKey::Set(pubkey, legacy)`): the
    /// probe of `HasOperatorKeyUnderAnyScheme`.
    pub fn operator_point(k: &OperatorPoint, scheme: BlsSchemeKind) -> Self {
        UniqueKey(Box::from(&OperatorKey::from_value(k.clone(), scheme).to_bytes()[..]))
    }

    /// The pre-image bytes; `SHA256d` of them is Core's `GetUniquePropertyHash`.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A property value as `Add`/`Delete`/`UpdateUniqueProperty` see it: its key, and whether it
/// equals the type's null value `T{}` (those functions refuse a null value, v24:552-555,
/// 572-575).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prop {
    pub(crate) key: UniqueKey,
    pub(crate) null: bool,
}

impl Prop {
    /// The collateral outpoint's property; null is Core's `COutPoint::IsNull()`.
    pub(crate) fn collateral(op: &OutPoint) -> Prop {
        Prop {
            key: UniqueKey::collateral(op),
            null: op.hash.is_null() && op.index == u32::MAX,
        }
    }
}

/// One `GetEntries()` entry as `AddMN`/`UpdateMN`/`RemoveMN` probe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NetProp {
    /// An address (`GetAddrPort`) or a domain (`GetDomainPort`), with Core's
    /// `ToStringAddrPort` text for the error messages.
    Addr {
        prop: Prop,
        shown: String,
    },
    /// Neither: Core throws "invalid address".
    Invalid,
}

/// The unique-property values a cold state holds, in the order `AddMN` adds them (EV2 §2.5),
/// built once per state ([`MnCold::unique_keys`]). The collateral is the `Dmn`'s, and whether the
/// platformNodeID counts depends on the `Dmn`'s type, so both are applied by the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StateKeys {
    /// `netInfo->GetEntries()`, in Core's order.
    pub(crate) net: Box<[NetProp]>,
    /// Every share's owner key for a shared masternode, else `keyIDOwner`.
    pub(crate) owners: Box<[Prop]>,
    /// The operator key; `null` is Core's `== CBLSLazyPublicKey()` (the default object), which
    /// `AddMN` and `RemoveMN` skip.
    pub(crate) operator: Prop,
    /// `platformNodeID`.
    pub(crate) platform: Prop,
}

impl StateKeys {
    /// The keys of `cold`, computed afresh (the reference twin does so on every operation).
    pub(crate) fn build(cold: &MnCold) -> StateKeys {
        let net = net_props(&cold.net_info);
        let owners = if cold.is_shared() {
            cold.shares.iter().map(|share| key_id_prop(&share.key_id_owner)).collect()
        } else {
            Box::from([key_id_prop(&cold.key_id_owner)])
        };
        let operator = Prop {
            key: UniqueKey::operator_key(&cold.pub_key_operator),
            // Comparing against the default object never decodes (`bls.h` v24:514).
            null: cold.pub_key_operator == OperatorKey::new(BlsSchemeKind::Basic),
        };
        let platform = Prop {
            key: UniqueKey::platform_node_id(&cold.platform_node_id),
            null: cold.platform_node_id.is_null(),
        };
        StateKeys {
            net,
            owners,
            operator,
            platform,
        }
    }
}

pub(crate) fn key_id_prop(k: &KeyId) -> Prop {
    Prop {
        key: UniqueKey::key_id(k),
        null: k.as_bytes().iter().all(|b| *b == 0),
    }
}

/// `GetEntries()` (`netinfo.cpp` v24:332-341, 553-560): a legacy service unless empty, or every
/// extended entry in purpose order (none for a version Core does not know).
pub(crate) fn net_props(net: &NetInfo) -> Box<[NetProp]> {
    match net {
        NetInfo::Legacy(v1) => {
            let service = &v1.0;
            if service.addr.is_null() && service.port == 0 {
                Box::new([])
            } else {
                Box::new([NetProp::Addr {
                    prop: Prop {
                        key: UniqueKey::service_v1(service),
                        null: false,
                    },
                    shown: service.to_string(),
                }])
            }
        }
        NetInfo::Extended(v2) => v2
            .entries
            .iter()
            .flat_map(|(_, group)| group)
            .map(|entry| match entry {
                NIEntry::Service(service) => NetProp::Addr {
                    prop: Prop {
                        key: UniqueKey::service(service),
                        // `CService{}` is `[::]:0`.
                        null: service.addr == AddrV2::Ipv6([0; 16]) && service.port == 0,
                    },
                    shown: service.to_string(),
                },
                NIEntry::Domain {
                    name,
                    port,
                } => NetProp::Addr {
                    prop: Prop {
                        key: UniqueKey::domain(name, *port),
                        null: name.is_empty() && *port == 0,
                    },
                    shown: format!("{}:{port}", String::from_utf8_lossy(name)),
                },
                NIEntry::Invalid => NetProp::Invalid,
            })
            .collect(),
    }
}

impl MnCold {
    /// The unique-property values this state holds, built once.
    pub(crate) fn unique_keys(&self) -> &Arc<StateKeys> {
        self.derived.unique_keys.get_or_init(|| Arc::new(StateKeys::build(self)))
    }
}

/// A slot of the map: the owning masternode and Core's reference count (EV2 §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UniqueSlot {
    /// The masternode whose property this is.
    pub owner: ProTxHash,
    /// How many of the owner's properties serialize to this key (above 1 only when one
    /// masternode holds equal bytes twice).
    pub refs: u32,
}

/// Core's `mnUniquePropertyMap`: probed, compared and snapshotted, never iterated or serialized
/// (EV2 §2.2).
///
/// imbl 7.0.2's `HashMap` is keyed SipHash-1-3 with a per-process random key: the key bytes are
/// attacker-chosen, and a fixed hasher admits cheap collision families (EV2 rev1, GPT F3).
/// Nothing observes the table's layout, because the newtype exposes no iteration, so the random
/// seed is unobservable and the determinism rule (A1 §2.1) holds. Equality is content equality.
/// Fuzz builds fix the key instead (`UniqueHasher`, BL-370).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct UniqueMap(imbl::GenericHashMap<UniqueKey, UniqueSlot, UniqueHasher, DefaultSharedPtr>);

/// [`UniqueMap`]'s hasher: `imbl::HashMap`'s own, the per-process random SipHash-1-3 key.
#[cfg(not(fuzzing))]
type UniqueHasher = std::collections::hash_map::RandomState;
/// Under `cfg(fuzzing)` only, std's fixed-key `DefaultHasher` (BL-370, DEC-390): with a random
/// key the table's shape, and so the coverage it drives, differs per process, and replays of one
/// corpus disagree on the PC set. Fuzz builds take their keys from the fuzzer, not the network.
#[cfg(fuzzing)]
type UniqueHasher = std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>;

impl UniqueMap {
    /// The slot for `key` (Core's `find`).
    pub fn get(&self, key: &UniqueKey) -> Option<&UniqueSlot> {
        self.0.get(key)
    }

    /// Whether `key` has a slot (Core's `count`).
    pub fn contains(&self, key: &UniqueKey) -> bool {
        self.0.contains_key(key)
    }

    /// The number of slots.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the map has no slot.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `AddUniqueProperty(dmn, v)`: false for a null value or a slot another masternode owns;
    /// the owner's own slot gains a reference.
    // Core: evo/deterministicmns.h:550 CDeterministicMNList::AddUniqueProperty
    pub(crate) fn add(&mut self, prop: &Prop, owner: &ProTxHash) -> bool {
        if prop.null {
            return false;
        }
        let refs = match self.0.get(&prop.key) {
            Some(slot) if slot.owner != *owner => return false,
            Some(slot) => slot.refs + 1,
            None => 1,
        };
        self.0.insert(
            prop.key.clone(),
            UniqueSlot {
                owner: *owner,
                refs,
            },
        );
        true
    }

    /// `DeleteUniqueProperty(dmn, v)`: false for a null value or a slot the masternode does not
    /// own; the last reference erases the slot.
    // Core: evo/deterministicmns.h:570 CDeterministicMNList::DeleteUniqueProperty
    pub(crate) fn delete(&mut self, prop: &Prop, owner: &ProTxHash) -> bool {
        if prop.null {
            return false;
        }
        match self.0.get(&prop.key) {
            Some(slot) if slot.owner == *owner => {
                if slot.refs == 1 {
                    self.0.remove(&prop.key);
                } else {
                    let refs = slot.refs - 1;
                    self.0.insert(
                        prop.key.clone(),
                        UniqueSlot {
                            owner: *owner,
                            refs,
                        },
                    );
                }
                true
            }
            _ => false,
        }
    }

    /// `UpdateUniqueProperty(dmn, old, new)` once the caller found `old != new`: a non-null old
    /// value is deleted, then a non-null new value added.
    // Core: evo/deterministicmns.h:589 CDeterministicMNList::UpdateUniqueProperty
    pub(crate) fn replace(&mut self, old: &Prop, new: &Prop, owner: &ProTxHash) -> bool {
        (old.null || self.delete(old, owner)) && (new.null || self.add(new, owner))
    }
}

impl std::fmt::Debug for UniqueMap {
    /// The slot count only: listing the slots would iterate the seeded table (EV2 §2.2).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UniqueMap({} slots)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use dash_primitives::{AddrV1, TxHash};

    use super::*;

    #[test]
    fn fixed_layout_keys_are_core_serializations() {
        let mut txid = [0u8; 32];
        for (i, b) in txid.iter_mut().enumerate() {
            *b = i as u8;
        }
        let op = OutPoint {
            hash: TxHash::from(txid),
            index: 0x0102_0304,
        };
        let key = UniqueKey::collateral(&op);
        // The txid's internal bytes, then n little-endian.
        assert_eq!(&key.as_bytes()[..32], &txid);
        assert_eq!(&key.as_bytes()[32..], &[4, 3, 2, 1]);

        // quirk 23: a key id and a platformNodeID with equal bytes share one slot.
        let mut id = [0u8; 20];
        id[0] = 0xaa;
        id[19] = 0x55;
        let key_id = UniqueKey::key_id(&KeyId::from(id));
        assert_eq!(key_id.as_bytes(), &id);
        assert_eq!(key_id, UniqueKey::platform_node_id(&PlatformNodeId::from(id)));
    }

    /// The ADDRv1 form for IPv4 and IPv6, the ADDRv2 form for the rest (quirk 23's length
    /// classes: a CJDNS service is 20 bytes like a key id, a TORv3 one 36 like an outpoint).
    #[test]
    fn services_hash_in_cores_two_formats() {
        let v4 = ServiceV2 {
            addr: AddrV2::Ipv4([1, 2, 3, 4]),
            port: 9999,
        };
        let key = UniqueKey::service(&v4);
        assert_eq!(
            key.as_bytes(),
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, 4, 0x27, 0x0f]
        );
        // The same service held by a legacy MnNetInfo takes the same slot.
        let mut mapped = [0u8; 16];
        mapped[10..12].copy_from_slice(&[0xff, 0xff]);
        mapped[12..].copy_from_slice(&[1, 2, 3, 4]);
        let v1 = ServiceV1 {
            addr: AddrV1::from_bytes(mapped),
            port: 9999,
        };
        assert_eq!(UniqueKey::service_v1(&v1), key);

        let cjdns = ServiceV2 {
            addr: AddrV2::Cjdns([0xfc; 16]),
            port: 1,
        };
        let key = UniqueKey::service(&cjdns);
        assert_eq!(key.as_bytes().len(), 20);
        assert_eq!(&key.as_bytes()[..2], &[6, 16]);
        let tor = ServiceV2 {
            addr: AddrV2::TorV3([7; 32]),
            port: 1,
        };
        assert_eq!(UniqueKey::service(&tor).as_bytes().len(), 36);
        assert_eq!(UniqueKey::domain(b"a.example", 443).as_bytes()[0], 9);
    }

    #[test]
    fn slots_follow_cores_add_and_delete_rules() {
        let (a, b) = (ProTxHash::from([1u8; 32]), ProTxHash::from([2u8; 32]));
        let prop = |byte: u8, null: bool| Prop {
            key: UniqueKey::key_id(&KeyId::from([byte; 20])),
            null,
        };
        let mut map = UniqueMap::default();
        assert!(!map.add(&prop(0, true), &a), "a null value is refused");
        assert!(map.add(&prop(1, false), &a));
        assert!(map.add(&prop(1, false), &a), "the owner adds a reference");
        assert_eq!(map.get(&prop(1, false).key).map(|s| s.refs), Some(2));
        assert!(!map.add(&prop(1, false), &b), "another owner is refused");
        assert!(!map.delete(&prop(1, false), &b));
        assert!(map.delete(&prop(1, false), &a));
        assert_eq!(map.get(&prop(1, false).key).map(|s| s.refs), Some(1));
        assert!(map.delete(&prop(1, false), &a));
        assert!(map.is_empty());
        assert!(!map.delete(&prop(1, false), &a), "an absent slot cannot be deleted");
        // A null side of an update is skipped.
        assert!(map.replace(&prop(0, true), &prop(2, false), &a));
        assert!(map.replace(&prop(2, false), &prop(0, true), &a));
        assert!(map.is_empty());
    }

    /// BL-370: two maps hash a key alike under `cfg(fuzzing)` only; otherwise each map draws its
    /// own random key (EV2 §2.2).
    #[test]
    fn hasher_key_is_fixed_only_under_fuzzing() {
        use std::hash::BuildHasher;
        let key = UniqueKey::key_id(&KeyId::from([7u8; 20]));
        let (a, b) = (UniqueMap::default(), UniqueMap::default());
        let same = a.0.hasher().hash_one(&key) == b.0.hasher().hash_one(&key);
        assert_eq!(same, cfg!(fuzzing));
    }
}
