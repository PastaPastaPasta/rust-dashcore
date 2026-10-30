// Rust Dash Library
// Originally written in 2014 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//     For Dash
// Updated for Dash in 2022 by
//     The Dash Core Developers
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the CC0 Public Domain Dedication
// along with this software.
// If not, see <http://creativecommons.org/publicdomain/zero/1.0/>.
//

//! Dash network constants.
//!
//! This module provides various constants relating to the Dash network
//! protocol.

use core::convert::From;
use core::{fmt, ops};

use hashes::Hash;

pub use dash_network::{Network, ParseNetworkError};

use crate::consensus::encode::{self, Decodable, Encodable};
use crate::{BlockHash, io};

// Re-export NODE_HEADERS_COMPRESSED for convenience
pub const NODE_HEADERS_COMPRESSED: ServiceFlags = ServiceFlags::NODE_HEADERS_COMPRESSED;

// The protocol-version constants below are Dash Core v24's `src/version.h`, name for name and in
// its order. `dash/tests/core_v24_protocol_tables.rs` checks them against the extracted table.

/// Version of the protocol as appearing in network message headers (Dash Core v24's
/// `PROTOCOL_VERSION`).
///
/// This constant is used to signal to other peers which features you support. Increasing it
/// implies that your software also supports every feature prior to this version, up to the
/// feature gates listed below. Doing so without support may lead to you incorrectly banning other
/// peers or other peers banning you.
pub const PROTOCOL_VERSION: u32 = 70242;

/// Initial protocol version, to be increased after version/verack negotiation.
pub const INIT_PROTO_VERSION: u32 = 209;

/// Peers older than this protocol version are disconnected.
pub const MIN_PEER_PROTO_VERSION: u32 = 70221;

/// Minimum protocol version of a masternode to accept in DKGs.
pub const MIN_MASTERNODE_PROTO_VERSION: u32 = 70242;

/// Protocol version that introduced `addrv2` (BIP155).
pub const ADDRV2_PROTO_VERSION: u32 = 70223;

/// Protocol version that introduced the BLS scheme.
pub const BLS_SCHEME_PROTO_VERSION: u32 = 70225;

/// Protocol version that introduced the masternode type.
pub const DMN_TYPE_PROTO_VERSION: u32 = 70227;

/// Protocol version that introduced versioned simplified masternode list entries.
pub const SMNLE_VERSIONED_PROTO_VERSION: u32 = 70228;

/// Protocol version that settled the field order of `mnlistdiff`'s version.
pub const MNLISTDIFF_VERSION_ORDER: u32 = 70229;

/// Protocol version that added chain locks to `mnlistdiff`.
pub const MNLISTDIFF_CHAINLOCKS_PROTO_VERSION: u32 = 70230;

/// Protocol version from which legacy ISLOCK messages and their inventory type were dropped.
pub const NO_LEGACY_ISLOCK_PROTO_VERSION: u32 = 70231;

/// Protocol version that added the inventory type for `dsq` messages
/// ([`Inventory::Dsq`](crate::network::message_blockdata::Inventory::Dsq)).
pub const DSQ_INV_VERSION: u32 = 70234;

/// Protocol version from which `headers2` may carry 8000 headers instead of 2000.
pub const INCREASE_MAX_HEADERS2_VERSION: u32 = 70235;

/// Protocol version that introduced the BIP324 v2 transport with Dash's short message IDs
/// (128-167).
pub const BIP324_DASH_BASELINE_VERSION: u32 = 70235;

/// Protocol version that changed the behavior of `qrinfo`.
pub const EFFICIENT_QRINFO_VERSION: u32 = 70236;

/// Protocol version from which the `cycleHash` of an `isdlock` is its quorum's base block.
pub const ISDLOCK_CYCLEHASH_UPDATE_VERSION: u32 = 70237;

/// Protocol version that introduced the `platformban` message.
pub const PLATFORM_BAN_VERSION: u32 = 70238;

/// Protocol version that banned re-propagation of old `qfcommit` enforcement.
pub const QFCOMMIT_STALE_REPROP_BAN_VERSION: u32 = 70239;

/// Protocol version that gave `platformban` the v2 short ID 168.
pub const PLATFORMBAN_V2_SHORT_ID_VERSION: u32 = 70240;

/// Protocol version from which CoinJoin `dsa` messages carry a flags field (denomination
/// promotion/demotion sessions, Dash Core v24).
pub const COINJOIN_REBALANCE_VERSION: u32 = 70241;

/// Protocol version from which version 2 asset unlocks are announced by instance hash with the
/// `MSG_ASSET_UNLOCK` inventory type ([`Inventory::AssetUnlock`]); peers below it are announced
/// the txid as a plain transaction (Dash Core v24).
///
/// [`Inventory::AssetUnlock`]: crate::network::message_blockdata::Inventory::AssetUnlock
pub const ASSET_UNLOCK_INV_VERSION: u32 = 70242;

/// Mainnet height from which Dash HD/BIP39 wallets could first exist on chain.
///
/// No address any HD wallet derives can appear earlier, so an SPV client scanning for HD
/// wallet activity has no reason to sync mainnet below this height. Matches the entry in
/// DashSync's `mainnet_checkpoint_array` commented "first sync time (aka BIP39 creation
/// time)", the value the official Dash mobile SPV client uses as its floor.
pub const MAINNET_HD_ACTIVATION_HEIGHT: u32 = 227121;

pub trait NetworkExt {
    /// Returns the known genesis block hash for `network`, if one is hardcoded.
    fn known_genesis_block_hash(&self) -> Option<BlockHash>;

    /// Earliest block height worth syncing for HD/BIP39 wallets, or `0` when the network
    /// has no floor (sync from genesis). Mainnet is floored at
    /// [`MAINNET_HD_ACTIVATION_HEIGHT`]; other networks return `0`.
    fn hd_wallet_sync_floor(&self) -> u32;
}

impl NetworkExt for Network {
    fn known_genesis_block_hash(&self) -> Option<BlockHash> {
        match self {
            Network::Mainnet => {
                let mut block_hash = hex_conservative::decode_to_vec(
                    "00000ffd590b1485b3caadc19b22e6379c733355108f107a430458cdf3407ab6",
                )
                .expect("expected valid hex");
                block_hash.reverse();
                Some(BlockHash::from_byte_array(block_hash.try_into().expect("expected 32 bytes")))
            }
            Network::Testnet => {
                let mut block_hash = hex_conservative::decode_to_vec(
                    "00000bafbc94add76cb75e2ec92894837288a481e5c005f6563d91623bf8bc2c",
                )
                .expect("expected valid hex");
                block_hash.reverse();
                Some(BlockHash::from_byte_array(block_hash.try_into().expect("expected 32 bytes")))
            }
            Network::Devnet => {
                let mut block_hash = hex_conservative::decode_to_vec(
                    "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e",
                )
                .expect("expected valid hex");
                block_hash.reverse();
                Some(BlockHash::from_byte_array(block_hash.try_into().expect("expected 32 bytes")))
            }
            Network::Regtest => {
                let mut block_hash = hex_conservative::decode_to_vec(
                    "000008ca1832a4baf228eb1553c03d3a2c8e02399550dd6ea8d65cec3ef23d2e",
                )
                .expect("expected valid hex");
                block_hash.reverse();
                Some(BlockHash::from_byte_array(block_hash.try_into().expect("expected 32 bytes")))
            }
        }
    }

    fn hd_wallet_sync_floor(&self) -> u32 {
        match self {
            Network::Mainnet => MAINNET_HD_ACTIVATION_HEIGHT,
            _ => 0,
        }
    }
}
/// Flags to indicate which network services a node supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceFlags(u64);

impl ServiceFlags {
    /// NONE means no services supported.
    pub const NONE: ServiceFlags = ServiceFlags(0);

    /// NETWORK means that the node is capable of serving the complete block chain. It is currently
    /// set by all Dash Core non pruned nodes, and is unset by SPV clients or other light
    /// clients.
    pub const NETWORK: ServiceFlags = ServiceFlags(1 << 0);

    /// BLOOM means the node is capable and willing to handle bloom-filtered connections.
    pub const BLOOM: ServiceFlags = ServiceFlags(1 << 2);

    /// COMPACT_FILTERS means the node will service basic block filter requests.
    /// See BIP157 and BIP158 for details on how this is implemented.
    pub const COMPACT_FILTERS: ServiceFlags = ServiceFlags(1 << 6);

    /// NETWORK_LIMITED means the same as NODE_NETWORK with the limitation of only serving the last
    /// 288 blocks.
    /// See BIP159 for details on how this is implemented.
    pub const NETWORK_LIMITED: ServiceFlags = ServiceFlags(1 << 10);

    /// HEADERS_COMPRESSED means the node supports compressed block headers as defined in DIP-0025.
    /// This allows for more efficient header synchronization by compressing headers from 80 bytes
    /// to as low as 37 bytes using stateful compression techniques.
    pub const HEADERS_COMPRESSED: ServiceFlags = ServiceFlags(1 << 11);

    /// Core's name for [`Self::HEADERS_COMPRESSED`] (`NODE_HEADERS_COMPRESSED`).
    pub const NODE_HEADERS_COMPRESSED: ServiceFlags = Self::HEADERS_COMPRESSED;

    /// P2P_V2 means the node supports the BIP324 transport.
    pub const P2P_V2: ServiceFlags = ServiceFlags(1 << 12);

    // NOTE: When adding new flags, remember to update `service_flag_name` accordingly. The set of
    // flags is Dash Core v24's `ServiceFlags` enum (src/protocol.h): bits 1 (GETUTXO) and 3
    // (WITNESS) of the Bitcoin flags are not Dash flags and render as unknown.

    /// Add [ServiceFlags] together.
    ///
    /// Returns itself.
    pub fn add(&mut self, other: ServiceFlags) -> ServiceFlags {
        self.0 |= other.0;
        *self
    }

    /// Remove [ServiceFlags] from this.
    ///
    /// Returns itself.
    pub fn remove(&mut self, other: ServiceFlags) -> ServiceFlags {
        self.0 ^= other.0;
        *self
    }

    /// Check whether [ServiceFlags] are included in this one.
    pub fn has(self, flags: ServiceFlags) -> bool {
        (self.0 | flags.0) == self.0
    }

    /// Get the integer representation of this [ServiceFlags].
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// The flags as human readable strings, one per set bit from the lowest, exactly as Dash
    /// Core's `serviceFlagsToStr` returns them (`NETWORK`, `BLOOM`, ..., `UNKNOWN[2^N]` for a
    /// bit that is not a Dash flag). No set bit gives an empty vector.
    pub fn to_strings(self) -> Vec<String> {
        (0..u64::BITS).filter(|bit| self.0 & (1u64 << bit) != 0).map(service_flag_name).collect()
    }
}

/// Dash Core's `serviceFlagToStr` for the flag `1 << bit`.
fn service_flag_name(bit: u32) -> String {
    match 1u64 << bit {
        f if f == ServiceFlags::NETWORK.0 => "NETWORK".to_string(),
        f if f == ServiceFlags::BLOOM.0 => "BLOOM".to_string(),
        f if f == ServiceFlags::COMPACT_FILTERS.0 => "COMPACT_FILTERS".to_string(),
        f if f == ServiceFlags::NETWORK_LIMITED.0 => "NETWORK_LIMITED".to_string(),
        f if f == ServiceFlags::HEADERS_COMPRESSED.0 => "HEADERS_COMPRESSED".to_string(),
        f if f == ServiceFlags::P2P_V2.0 => "P2P_V2".to_string(),
        _ => format!("UNKNOWN[2^{}]", bit),
    }
}

impl fmt::LowerHex for ServiceFlags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

impl fmt::UpperHex for ServiceFlags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, f)
    }
}

/// `ServiceFlags(` and the names of [`ServiceFlags::to_strings`] joined with `|`, as Dash Core
/// names them; `ServiceFlags(NONE)` when no bit is set.
impl fmt::Display for ServiceFlags {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if *self == ServiceFlags::NONE {
            return write!(f, "ServiceFlags(NONE)");
        }
        write!(f, "ServiceFlags({})", self.to_strings().join("|"))
    }
}

impl From<u64> for ServiceFlags {
    fn from(f: u64) -> Self {
        ServiceFlags(f)
    }
}

impl From<ServiceFlags> for u64 {
    fn from(val: ServiceFlags) -> Self {
        val.0
    }
}

impl ops::BitOr for ServiceFlags {
    type Output = Self;

    fn bitor(mut self, rhs: Self) -> Self {
        self.add(rhs)
    }
}

impl ops::BitOrAssign for ServiceFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.add(rhs);
    }
}

impl ops::BitXor for ServiceFlags {
    type Output = Self;

    fn bitxor(mut self, rhs: Self) -> Self {
        self.remove(rhs)
    }
}

impl ops::BitXorAssign for ServiceFlags {
    fn bitxor_assign(&mut self, rhs: Self) {
        self.remove(rhs);
    }
}

impl Encodable for ServiceFlags {
    #[inline]
    fn consensus_encode<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<usize, io::Error> {
        self.0.consensus_encode(w)
    }
}

impl Decodable for ServiceFlags {
    #[inline]
    fn consensus_decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, encode::Error> {
        Ok(ServiceFlags(Decodable::consensus_decode(r)?))
    }
}

#[cfg(test)]
mod tests {
    use super::{MAINNET_HD_ACTIVATION_HEIGHT, NetworkExt, ServiceFlags};
    use crate::Network;
    use crate::consensus::encode::{deserialize, serialize};

    #[test]
    fn test_hd_wallet_sync_floor() {
        // Only mainnet is floored; every other network syncs from genesis (0).
        assert_eq!(Network::Mainnet.hd_wallet_sync_floor(), MAINNET_HD_ACTIVATION_HEIGHT);
        assert_eq!(Network::Testnet.hd_wallet_sync_floor(), 0);
        assert_eq!(Network::Devnet.hd_wallet_sync_floor(), 0);
        assert_eq!(Network::Regtest.hd_wallet_sync_floor(), 0);
    }

    #[test]
    fn test_network_magic() {
        assert_eq!(Network::Mainnet.magic(), 0xBD6B0CBF);
        assert_eq!(Network::Testnet.magic(), 0xFFCAE2CE);
        assert_eq!(Network::Devnet.magic(), 0xCEFFCAE2);
        assert_eq!(Network::Regtest.magic(), 0xDCB7C1FC);
    }

    #[test]
    fn test_network_from_magic() {
        assert_eq!(Network::from_magic(0xBD6B0CBF), Some(Network::Mainnet));
        assert_eq!(Network::from_magic(0xFFCAE2CE), Some(Network::Testnet));
        assert_eq!(Network::from_magic(0xCEFFCAE2), Some(Network::Devnet));
        assert_eq!(Network::from_magic(0xDCB7C1FC), Some(Network::Regtest));
        assert_eq!(Network::from_magic(0x12345678), None);
    }

    #[test]
    fn test_network_display() {
        assert_eq!(Network::Mainnet.to_string(), "mainnet");
        assert_eq!(Network::Testnet.to_string(), "testnet");
        assert_eq!(Network::Devnet.to_string(), "devnet");
        assert_eq!(Network::Regtest.to_string(), "regtest");
    }

    #[test]
    fn test_network_from_str() {
        assert_eq!("mainnet".parse::<Network>().unwrap(), Network::Mainnet);
        assert_eq!("testnet".parse::<Network>().unwrap(), Network::Testnet);
        assert_eq!("test".parse::<Network>().unwrap(), Network::Testnet);
        assert_eq!("devnet".parse::<Network>().unwrap(), Network::Devnet);
        assert_eq!("dev".parse::<Network>().unwrap(), Network::Devnet);
        assert_eq!("regtest".parse::<Network>().unwrap(), Network::Regtest);
        assert!("invalid".parse::<Network>().is_err());
    }

    #[test]
    fn serialize_test() {
        assert_eq!(serialize(&Network::Mainnet.magic()), &[0xbf, 0x0c, 0x6b, 0xbd]);
        assert_eq!(serialize(&Network::Testnet.magic()), &[0xce, 0xe2, 0xca, 0xff]);
        assert_eq!(serialize(&Network::Devnet.magic()), &[0xe2, 0xca, 0xff, 0xce]);
        assert_eq!(serialize(&Network::Regtest.magic()), &[0xfc, 0xc1, 0xb7, 0xdc]);

        assert_eq!(deserialize(&[0xbf, 0x0c, 0x6b, 0xbd]).ok(), Some(Network::Mainnet.magic()));
        assert_eq!(deserialize(&[0xce, 0xe2, 0xca, 0xff]).ok(), Some(Network::Testnet.magic()));
        assert_eq!(deserialize(&[0xe2, 0xca, 0xff, 0xce]).ok(), Some(Network::Devnet.magic()));
        assert_eq!(deserialize(&[0xfc, 0xc1, 0xb7, 0xdc]).ok(), Some(Network::Regtest.magic()));
    }

    #[test]
    fn service_flags_test() {
        let all = [
            ServiceFlags::NETWORK,
            ServiceFlags::BLOOM,
            ServiceFlags::COMPACT_FILTERS,
            ServiceFlags::NETWORK_LIMITED,
            ServiceFlags::HEADERS_COMPRESSED,
            ServiceFlags::P2P_V2,
        ];

        let mut flags = ServiceFlags::NONE;
        for f in all.iter() {
            assert!(!flags.has(*f));
        }

        flags |= ServiceFlags::BLOOM;
        assert_eq!(flags, ServiceFlags::BLOOM);

        let mut flags2 = flags | ServiceFlags::P2P_V2;
        for f in all.iter() {
            assert_eq!(flags2.has(*f), *f == ServiceFlags::BLOOM || *f == ServiceFlags::P2P_V2);
        }

        flags2 ^= ServiceFlags::BLOOM;
        assert_eq!(flags2, ServiceFlags::P2P_V2);

        flags2 |= ServiceFlags::COMPACT_FILTERS;
        flags2 ^= ServiceFlags::P2P_V2;
        assert_eq!(flags2, ServiceFlags::COMPACT_FILTERS);

        // Core's names, in bit order; bits that are not Dash flags are UNKNOWN[2^N].
        assert_eq!("ServiceFlags(NONE)", ServiceFlags::NONE.to_string());
        assert_eq!("ServiceFlags(BLOOM)", ServiceFlags::BLOOM.to_string());
        let flag = ServiceFlags::BLOOM | ServiceFlags::NETWORK | ServiceFlags::HEADERS_COMPRESSED;
        assert_eq!("ServiceFlags(NETWORK|BLOOM|HEADERS_COMPRESSED)", flag.to_string());
        let flag = ServiceFlags::NETWORK | 0xf0.into();
        assert_eq!(
            "ServiceFlags(NETWORK|UNKNOWN[2^4]|UNKNOWN[2^5]|COMPACT_FILTERS|UNKNOWN[2^7])",
            flag.to_string()
        );
        // Bitcoin's GETUTXO (bit 1) and WITNESS (bit 3) are not Dash flags.
        assert_eq!(ServiceFlags::from(0b1010).to_strings(), ["UNKNOWN[2^1]", "UNKNOWN[2^3]"]);
        assert!(ServiceFlags::NONE.to_strings().is_empty());
        assert_eq!(ServiceFlags::NODE_HEADERS_COMPRESSED, ServiceFlags::HEADERS_COMPRESSED);
    }
}
