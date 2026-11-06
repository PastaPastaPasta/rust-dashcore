//! Core's `CSimplifiedMNListEntry` and its hash, the leaf of `merkleRootMNList`.

use dash_consensus::hash::HashWriter;
use dash_consensus::primitives::{BlockHash, CoreWrite, write_bytes, write_compact_size};
use dash_num::Hash256;
use dash_primitives::{NIEntry, NetInfo, NetInfoV1, NetInfoV2, PlatformNodeId};
use dash_types::Numeric;

use crate::types::{ProTxHash, RawMnType};

/// The entry version from which an entry carries its type (`ProTxVersion::BasicBLS`).
pub const SML_VERSION_BASIC_BLS: u16 = 2;

/// The entry version from which the network info is extended and an Evo entry carries no
/// platform HTTP port (`ProTxVersion::ExtAddr`).
pub const SML_VERSION_EXT_ADDR: u16 = 3;

/// One entry of the simplified masternode list (Core's `CSimplifiedMNListEntry`,
/// `evo/simplifiedmns.h` v24:28-104), with the fields Core serializes.
///
/// The node projects one from each masternode (`CDeterministicMN::to_sml_entry`, EV4-serve);
/// SPV reads them from `mnlistdiff` (through `dash-spv::bridge`). Core's mem-only fields (payout
/// scripts and shares) are not part of the entry's hash and are not kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmlEntry {
    /// `nVersion`. It selects the layout but is not part of the hash.
    pub version: u16,
    /// `proRegTxHash`.
    pub pro_reg_tx_hash: ProTxHash,
    /// `confirmedHash`; zero until the registration is confirmed.
    pub confirmed_hash: BlockHash,
    /// `netInfo`: the legacy service below [`SML_VERSION_EXT_ADDR`], the extended network info
    /// from it. Core writes it as it holds it, an address it did not keep as the empty one, which
    /// base-sdk's decoders reproduce.
    pub net_info: NetInfo,
    /// `pubKeyOperator` as written in the entry's scheme (legacy for version 1, basic
    /// otherwise): the bytes Core hashes ("SML entry hash over the raw bytes", A1 §1.4.1).
    pub operator_key: [u8; 48],
    /// `keyIDVoting`.
    pub voting_key_id: [u8; 20],
    /// `isValid`: the masternode is not PoSe-banned.
    pub is_valid: bool,
    /// `nType`, carried from [`SML_VERSION_BASIC_BLS`].
    pub mn_type: RawMnType,
    /// `platformHTTPPort`, carried by an Evo entry below [`SML_VERSION_EXT_ADDR`].
    pub platform_http_port: u16,
    /// `platformNodeID`, carried by an Evo entry.
    pub platform_node_id: PlatformNodeId,
}

impl SmlEntry {
    /// Core's `CalcHash()`: the SHA256d of the entry's `SER_GETHASH` serialization, which leaves
    /// out the network-only `nVersion` (`evo/simplifiedmns.cpp` v24:46-51).
    // Core: evo/simplifiedmns.cpp:46 CSimplifiedMNListEntry::CalcHash
    pub fn hash(&self) -> Hash256 {
        let mut writer = HashWriter::new();
        self.write_hashed(&mut writer);
        Hash256::from(writer.finish())
    }

    /// The entry's serialization without `nVersion`: the hash pre-image, and the wire body after
    /// it.
    // Core: evo/simplifiedmns.h:72 CSimplifiedMNListEntry SERIALIZE_METHODS
    pub fn write_hashed(&self, w: &mut impl CoreWrite) {
        w.write(self.pro_reg_tx_hash.as_bytes());
        w.write(self.confirmed_hash.as_bytes());
        write_net_info(&self.net_info, w);
        w.write(&self.operator_key);
        w.write(&self.voting_key_id);
        w.write(&[u8::from(self.is_valid)]);
        if self.version >= SML_VERSION_BASIC_BLS {
            w.write(&self.mn_type.0.to_le_bytes());
            if self.mn_type.is_evo() {
                if self.version < SML_VERSION_EXT_ADDR {
                    w.write(&self.platform_http_port.to_le_bytes());
                }
                w.write(self.platform_node_id.as_bytes());
            }
        }
    }
}

/// Core's serialization of a masternode's network info (`NetInfoSerWrapper`,
/// `evo/netinfo.h` v24:470-515): the legacy service as an ADDRv1 `CService`, the extended
/// form as `ExtNetInfo`. The DML's canonical encoders write the same bytes (EV2 §2.9).
// Core: evo/netinfo.h:327 MnNetInfo::Serialize, :412 ExtNetInfo::Serialize
pub fn write_net_info(net_info: &NetInfo, w: &mut impl CoreWrite) {
    match net_info {
        NetInfo::Legacy(NetInfoV1(service)) => {
            w.write(&service.addr.0);
            w.write(&service.port.to_be_bytes());
        }
        NetInfo::Extended(v2) => write_ext_net_info(v2, w),
    }
}

// Core: evo/netinfo.h:412 ExtNetInfo::Serialize; :228 NetInfoEntry::Serialize (ADDRV2_FORMAT)
fn write_ext_net_info(info: &NetInfoV2, w: &mut impl CoreWrite) {
    w.write(&[info.version]);
    // Core writes nothing past a version it does not know (`ExtNetInfo::CURRENT_VERSION` is 1).
    if info.version == 0 || info.version > 1 {
        return;
    }
    write_compact_size(info.entries.len() as u64, w);
    for (purpose, group) in &info.entries {
        w.write(&[purpose.to_base()]);
        write_compact_size(group.len() as u64, w);
        for entry in group {
            match entry {
                NIEntry::Service(service) => {
                    w.write(&[ENTRY_SERVICE, service.addr.network().to_base()]);
                    write_bytes(service.addr.bytes(), w);
                    w.write(&service.port.to_be_bytes());
                }
                NIEntry::Domain {
                    name,
                    port,
                } => {
                    w.write(&[ENTRY_DOMAIN]);
                    write_bytes(name, w);
                    w.write(&port.to_be_bytes());
                }
                NIEntry::Invalid => w.write(&[ENTRY_INVALID]),
            }
        }
    }
}

/// `NetInfoType::Service`, `Domain` and `Invalid` (`evo/netinfo.h` v24).
const ENTRY_SERVICE: u8 = 0x01;
const ENTRY_DOMAIN: u8 = 0x02;
const ENTRY_INVALID: u8 = 0xff;
