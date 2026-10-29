// Rust Dash Library
// Originally written in 2014 by
//     Andrew Poelstra <apoelstra@wpsoftware.net>
//     For Bitcoin
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

//! Bitcoin blockdata network messages.
//!
//! This module describes network messages which are used for passing
//! Bitcoin data (blocks and transactions) around.
//!

use core::fmt;
use std::io;

use hashes::sha256d;

use crate::consensus::encode::{self, Decodable, Encodable};
use crate::hash_types::{BlockHash, ChainLockHash, InstantSendLockHash, Txid};
use crate::hashes::Hash;
use crate::internal_macros::impl_consensus_encoding;
use crate::network::constants;
use crate::prelude::*;

/// The inventory type numbers: the `GetDataMsg` enum of Dash Core v24's `src/protocol.h`, with
/// Core's identifiers. Numbers Core's enum leaves out (4, 5, 7-15, 19, 22, 27, 30, and anything
/// above 34, BIP144's `0x40000000` flag included) are not inventory types.
pub mod inv_type {
    /// `UNDEFINED`
    pub const UNDEFINED: u32 = 0;
    /// `MSG_TX`
    pub const MSG_TX: u32 = 1;
    /// `MSG_BLOCK`
    pub const MSG_BLOCK: u32 = 2;
    /// `MSG_FILTERED_BLOCK` (BIP37); only occurs in `getdata`.
    pub const MSG_FILTERED_BLOCK: u32 = 3;
    /// `MSG_SPORK`
    pub const MSG_SPORK: u32 = 6;
    /// `MSG_DSTX`
    pub const MSG_DSTX: u32 = 16;
    /// `MSG_GOVERNANCE_OBJECT`
    pub const MSG_GOVERNANCE_OBJECT: u32 = 17;
    /// `MSG_GOVERNANCE_OBJECT_VOTE`
    pub const MSG_GOVERNANCE_OBJECT_VOTE: u32 = 18;
    /// `MSG_CMPCT_BLOCK` (BIP152); only occurs in `getdata`.
    pub const MSG_CMPCT_BLOCK: u32 = 20;
    /// `MSG_QUORUM_FINAL_COMMITMENT`
    pub const MSG_QUORUM_FINAL_COMMITMENT: u32 = 21;
    /// `MSG_QUORUM_CONTRIB`
    pub const MSG_QUORUM_CONTRIB: u32 = 23;
    /// `MSG_QUORUM_COMPLAINT`
    pub const MSG_QUORUM_COMPLAINT: u32 = 24;
    /// `MSG_QUORUM_JUSTIFICATION`
    pub const MSG_QUORUM_JUSTIFICATION: u32 = 25;
    /// `MSG_QUORUM_PREMATURE_COMMITMENT`
    pub const MSG_QUORUM_PREMATURE_COMMITMENT: u32 = 26;
    /// `MSG_QUORUM_RECOVERED_SIG`
    pub const MSG_QUORUM_RECOVERED_SIG: u32 = 28;
    /// `MSG_CLSIG`
    pub const MSG_CLSIG: u32 = 29;
    /// `MSG_ISDLOCK`
    pub const MSG_ISDLOCK: u32 = 31;
    /// `MSG_DSQ`
    pub const MSG_DSQ: u32 = 32;
    /// `MSG_PLATFORM_BAN`
    pub const MSG_PLATFORM_BAN: u32 = 33;
    /// `MSG_ASSET_UNLOCK`
    pub const MSG_ASSET_UNLOCK: u32 = 34;
}

/// Every `GetDataMsg` member of Dash Core v24, as (Core's identifier, number) in Core's order:
/// twenty entries, `UNDEFINED` included.
pub const INVENTORY_TYPES: [(&str, u32); 20] = [
    ("UNDEFINED", inv_type::UNDEFINED),
    ("MSG_TX", inv_type::MSG_TX),
    ("MSG_BLOCK", inv_type::MSG_BLOCK),
    ("MSG_FILTERED_BLOCK", inv_type::MSG_FILTERED_BLOCK),
    ("MSG_SPORK", inv_type::MSG_SPORK),
    ("MSG_DSTX", inv_type::MSG_DSTX),
    ("MSG_GOVERNANCE_OBJECT", inv_type::MSG_GOVERNANCE_OBJECT),
    ("MSG_GOVERNANCE_OBJECT_VOTE", inv_type::MSG_GOVERNANCE_OBJECT_VOTE),
    ("MSG_CMPCT_BLOCK", inv_type::MSG_CMPCT_BLOCK),
    ("MSG_QUORUM_FINAL_COMMITMENT", inv_type::MSG_QUORUM_FINAL_COMMITMENT),
    ("MSG_QUORUM_CONTRIB", inv_type::MSG_QUORUM_CONTRIB),
    ("MSG_QUORUM_COMPLAINT", inv_type::MSG_QUORUM_COMPLAINT),
    ("MSG_QUORUM_JUSTIFICATION", inv_type::MSG_QUORUM_JUSTIFICATION),
    ("MSG_QUORUM_PREMATURE_COMMITMENT", inv_type::MSG_QUORUM_PREMATURE_COMMITMENT),
    ("MSG_QUORUM_RECOVERED_SIG", inv_type::MSG_QUORUM_RECOVERED_SIG),
    ("MSG_CLSIG", inv_type::MSG_CLSIG),
    ("MSG_ISDLOCK", inv_type::MSG_ISDLOCK),
    ("MSG_DSQ", inv_type::MSG_DSQ),
    ("MSG_PLATFORM_BAN", inv_type::MSG_PLATFORM_BAN),
    ("MSG_ASSET_UNLOCK", inv_type::MSG_ASSET_UNLOCK),
];

/// The command Core ties to an inventory type number (`CInv::GetCommandInternal`), or `None` for
/// a number that is not a known inventory type. `MSG_ASSET_UNLOCK` is carried in a plain `tx`
/// message.
pub fn inv_type_command(inv_type: u32) -> Option<&'static str> {
    use self::inv_type::*;
    Some(match inv_type {
        MSG_TX => "tx",
        MSG_BLOCK => "block",
        MSG_FILTERED_BLOCK => "merkleblock",
        MSG_CMPCT_BLOCK => "cmpctblock",
        MSG_SPORK => "spork",
        MSG_DSTX => "dstx",
        MSG_GOVERNANCE_OBJECT => "govobj",
        MSG_GOVERNANCE_OBJECT_VOTE => "govobjvote",
        MSG_QUORUM_FINAL_COMMITMENT => "qfcommit",
        MSG_QUORUM_CONTRIB => "qcontrib",
        MSG_QUORUM_COMPLAINT => "qcomplaint",
        MSG_QUORUM_JUSTIFICATION => "qjustify",
        MSG_QUORUM_PREMATURE_COMMITMENT => "qpcommit",
        MSG_QUORUM_RECOVERED_SIG => "qsigrec",
        MSG_CLSIG => "clsig",
        MSG_ISDLOCK => "isdlock",
        MSG_DSQ => "dsq",
        MSG_PLATFORM_BAN => "platformban",
        MSG_ASSET_UNLOCK => "tx",
        _ => return None,
    })
}

/// An inventory item: Core's `CInv`, the type number and the 32-byte hash.
///
/// The variants are Dash Core v24's inventory types ([`INVENTORY_TYPES`]); every other number,
/// `UNDEFINED` (0) included, decodes as [`Inventory::Unknown`]. Dash has no BIP144 and no BIP339:
/// there are no witness or wtxid inventory types.
#[derive(PartialEq, Eq, Clone, Debug, Copy, Hash, PartialOrd, Ord)]
pub enum Inventory {
    /// `MSG_TX` (1): a transaction
    Transaction(Txid),
    /// `MSG_BLOCK` (2): a block
    Block(BlockHash),
    /// `MSG_FILTERED_BLOCK` (3): a filtered block (merkle block)
    FilteredBlock(BlockHash),
    /// `MSG_SPORK` (6): a spork
    Spork(sha256d::Hash),
    /// `MSG_DSTX` (16): a CoinJoin transaction, by its txid
    Dstx(Txid),
    /// `MSG_GOVERNANCE_OBJECT` (17)
    GovernanceObject(sha256d::Hash),
    /// `MSG_GOVERNANCE_OBJECT_VOTE` (18)
    GovernanceObjectVote(sha256d::Hash),
    /// `MSG_CMPCT_BLOCK` (20): a compact block; only occurs in `getdata`
    CompactBlock(BlockHash),
    /// `MSG_QUORUM_FINAL_COMMITMENT` (21)
    QuorumFinalCommitment(sha256d::Hash),
    /// `MSG_QUORUM_CONTRIB` (23)
    QuorumContrib(sha256d::Hash),
    /// `MSG_QUORUM_COMPLAINT` (24)
    QuorumComplaint(sha256d::Hash),
    /// `MSG_QUORUM_JUSTIFICATION` (25)
    QuorumJustification(sha256d::Hash),
    /// `MSG_QUORUM_PREMATURE_COMMITMENT` (26)
    QuorumPrematureCommitment(sha256d::Hash),
    /// `MSG_QUORUM_RECOVERED_SIG` (28)
    QuorumRecoveredSig(sha256d::Hash),
    /// `MSG_CLSIG` (29): a chain lock
    ChainLock(ChainLockHash),
    /// `MSG_ISDLOCK` (31): an InstantSend lock
    InstantSendLock(InstantSendLockHash),
    /// `MSG_DSQ` (32): a CoinJoin queue message. Only exchanged with peers at
    /// [`DSQ_INV_VERSION`](super::constants::DSQ_INV_VERSION) or above.
    Dsq(sha256d::Hash),
    /// `MSG_PLATFORM_BAN` (33): a Platform service ban (DIP-0031)
    PlatformBan(sha256d::Hash),
    /// `MSG_ASSET_UNLOCK` (34): a version 2 asset unlock, announced by its instance hash
    /// ([`Transaction::instance_hash`](crate::Transaction::instance_hash)) because re-signed
    /// instances share one txid. A `getdata` for it is answered with a `tx` message. Only
    /// exchanged with peers at [`ASSET_UNLOCK_INV_VERSION`](super::constants::ASSET_UNLOCK_INV_VERSION)
    /// or above.
    AssetUnlock(Txid),

    /// Any other type number: Core does not know it, and skips it. Decoding never produces this
    /// for a number that has a variant above, and encoding writes `inv_type` as it is, so a value
    /// built by hand with a known number encodes like that variant but does not compare equal.
    Unknown {
        /// The inventory item type.
        inv_type: u32,
        /// The hash of the inventory item
        hash: [u8; 32],
    },
}

impl Inventory {
    /// The type number on the wire.
    pub fn inv_type(&self) -> u32 {
        use self::inv_type::*;
        match *self {
            Inventory::Transaction(_) => MSG_TX,
            Inventory::Block(_) => MSG_BLOCK,
            Inventory::FilteredBlock(_) => MSG_FILTERED_BLOCK,
            Inventory::Spork(_) => MSG_SPORK,
            Inventory::Dstx(_) => MSG_DSTX,
            Inventory::GovernanceObject(_) => MSG_GOVERNANCE_OBJECT,
            Inventory::GovernanceObjectVote(_) => MSG_GOVERNANCE_OBJECT_VOTE,
            Inventory::CompactBlock(_) => MSG_CMPCT_BLOCK,
            Inventory::QuorumFinalCommitment(_) => MSG_QUORUM_FINAL_COMMITMENT,
            Inventory::QuorumContrib(_) => MSG_QUORUM_CONTRIB,
            Inventory::QuorumComplaint(_) => MSG_QUORUM_COMPLAINT,
            Inventory::QuorumJustification(_) => MSG_QUORUM_JUSTIFICATION,
            Inventory::QuorumPrematureCommitment(_) => MSG_QUORUM_PREMATURE_COMMITMENT,
            Inventory::QuorumRecoveredSig(_) => MSG_QUORUM_RECOVERED_SIG,
            Inventory::ChainLock(_) => MSG_CLSIG,
            Inventory::InstantSendLock(_) => MSG_ISDLOCK,
            Inventory::Dsq(_) => MSG_DSQ,
            Inventory::PlatformBan(_) => MSG_PLATFORM_BAN,
            Inventory::AssetUnlock(_) => MSG_ASSET_UNLOCK,
            Inventory::Unknown {
                inv_type,
                ..
            } => inv_type,
        }
    }

    /// The hash, in wire byte order.
    pub fn hash_bytes(&self) -> [u8; 32] {
        match *self {
            Inventory::Transaction(h) | Inventory::Dstx(h) | Inventory::AssetUnlock(h) => {
                h.to_byte_array()
            }
            Inventory::Block(h) | Inventory::FilteredBlock(h) | Inventory::CompactBlock(h) => {
                h.to_byte_array()
            }
            Inventory::Spork(h)
            | Inventory::GovernanceObject(h)
            | Inventory::GovernanceObjectVote(h)
            | Inventory::QuorumFinalCommitment(h)
            | Inventory::QuorumContrib(h)
            | Inventory::QuorumComplaint(h)
            | Inventory::QuorumJustification(h)
            | Inventory::QuorumPrematureCommitment(h)
            | Inventory::QuorumRecoveredSig(h)
            | Inventory::Dsq(h)
            | Inventory::PlatformBan(h) => h.to_byte_array(),
            Inventory::ChainLock(h) => h.to_byte_array(),
            Inventory::InstantSendLock(h) => h.to_byte_array(),
            Inventory::Unknown {
                hash,
                ..
            } => hash,
        }
    }

    /// Whether Core knows the type (`CInv::IsKnownType`); Core skips an item whose type it
    /// does not know.
    pub fn is_known_type(&self) -> bool {
        inv_type_command(self.inv_type()).is_some()
    }

    /// The command of the item's type (`CInv::GetCommand`), `None` for an unknown type (where
    /// Core throws).
    pub fn command(&self) -> Option<&'static str> {
        inv_type_command(self.inv_type())
    }
}

/// Core's `CInv::ToString`: `<command> <hash>` for a known type and `0x<type, 8 hex digits>
/// <hash>` for an unknown one, with the hash as a `uint256` prints it (byte-reversed hex).
impl fmt::Display for Inventory {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.command() {
            Some(cmd) => write!(f, "{} ", cmd)?,
            None => write!(f, "0x{:08x} ", self.inv_type())?,
        }
        for byte in self.hash_bytes().iter().rev() {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

impl Encodable for Inventory {
    #[inline]
    fn consensus_encode<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<usize, io::Error> {
        // `CInv`: the type as a u32, then the hash.
        Ok(self.inv_type().consensus_encode(w)? + self.hash_bytes().consensus_encode(w)?)
    }
}

impl Decodable for Inventory {
    #[inline]
    fn consensus_decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, encode::Error> {
        use self::inv_type::*;
        let inv_type: u32 = Decodable::consensus_decode(r)?;
        Ok(match inv_type {
            MSG_TX => Inventory::Transaction(Decodable::consensus_decode(r)?),
            MSG_BLOCK => Inventory::Block(Decodable::consensus_decode(r)?),
            MSG_FILTERED_BLOCK => Inventory::FilteredBlock(Decodable::consensus_decode(r)?),
            MSG_SPORK => Inventory::Spork(Decodable::consensus_decode(r)?),
            MSG_DSTX => Inventory::Dstx(Decodable::consensus_decode(r)?),
            MSG_GOVERNANCE_OBJECT => Inventory::GovernanceObject(Decodable::consensus_decode(r)?),
            MSG_GOVERNANCE_OBJECT_VOTE => {
                Inventory::GovernanceObjectVote(Decodable::consensus_decode(r)?)
            }
            MSG_CMPCT_BLOCK => Inventory::CompactBlock(Decodable::consensus_decode(r)?),
            MSG_QUORUM_FINAL_COMMITMENT => {
                Inventory::QuorumFinalCommitment(Decodable::consensus_decode(r)?)
            }
            MSG_QUORUM_CONTRIB => Inventory::QuorumContrib(Decodable::consensus_decode(r)?),
            MSG_QUORUM_COMPLAINT => Inventory::QuorumComplaint(Decodable::consensus_decode(r)?),
            MSG_QUORUM_JUSTIFICATION => {
                Inventory::QuorumJustification(Decodable::consensus_decode(r)?)
            }
            MSG_QUORUM_PREMATURE_COMMITMENT => {
                Inventory::QuorumPrematureCommitment(Decodable::consensus_decode(r)?)
            }
            MSG_QUORUM_RECOVERED_SIG => {
                Inventory::QuorumRecoveredSig(Decodable::consensus_decode(r)?)
            }
            MSG_CLSIG => Inventory::ChainLock(Decodable::consensus_decode(r)?),
            MSG_ISDLOCK => Inventory::InstantSendLock(Decodable::consensus_decode(r)?),
            MSG_DSQ => Inventory::Dsq(Decodable::consensus_decode(r)?),
            MSG_PLATFORM_BAN => Inventory::PlatformBan(Decodable::consensus_decode(r)?),
            MSG_ASSET_UNLOCK => Inventory::AssetUnlock(Decodable::consensus_decode(r)?),
            tp => Inventory::Unknown {
                inv_type: tp,
                hash: Decodable::consensus_decode(r)?,
            },
        })
    }
}

// Some simple messages

/// The `getblocks` message
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct GetBlocksMessage {
    /// The protocol version
    pub version: u32,
    /// Locator hashes --- ordered newest to oldest. The remote peer will
    /// reply with its longest known chain, starting from a locator hash
    /// if possible and block 1 otherwise.
    pub locator_hashes: Vec<BlockHash>,
    /// References the block to stop at, or zero to just fetch the maximum 500 blocks
    pub stop_hash: BlockHash,
}

/// The `getheaders` message
#[derive(PartialEq, Eq, Clone, Debug)]
pub struct GetHeadersMessage {
    /// The protocol version
    pub version: u32,
    /// Locator hashes --- ordered newest to oldest. The remote peer will
    /// reply with its longest known chain, starting from a locator hash
    /// if possible and block 1 otherwise.
    pub locator_hashes: Vec<BlockHash>,
    /// References the header to stop at, or zero to just fetch the maximum 2000 headers
    pub stop_hash: BlockHash,
}

impl GetBlocksMessage {
    /// Construct a new `getblocks` message
    pub fn new(locator_hashes: Vec<BlockHash>, stop_hash: BlockHash) -> GetBlocksMessage {
        GetBlocksMessage {
            version: constants::PROTOCOL_VERSION,
            locator_hashes,
            stop_hash,
        }
    }
}

impl_consensus_encoding!(GetBlocksMessage, version, locator_hashes, stop_hash);

impl GetHeadersMessage {
    /// Construct a new `getheaders` message
    pub fn new(locator_hashes: Vec<BlockHash>, stop_hash: BlockHash) -> GetHeadersMessage {
        GetHeadersMessage {
            version: constants::PROTOCOL_VERSION,
            locator_hashes,
            stop_hash,
        }
    }
}

impl_consensus_encoding!(GetHeadersMessage, version, locator_hashes, stop_hash);

#[cfg(test)]
mod tests {
    use hashes::Hash;

    use super::{GetBlocksMessage, GetHeadersMessage, Inventory, Vec};
    use crate::consensus::encode::{deserialize, serialize};
    use crate::internal_macros::hex;

    #[test]
    fn getblocks_message_test() {
        let from_sat: Vec<u8> = hex!(
            "72110100014a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b0000000000000000000000000000000000000000000000000000000000000000"
        );
        let genhash: Vec<u8> =
            hex!("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b");

        let decode: Result<GetBlocksMessage, _> = deserialize(&from_sat);
        assert!(decode.is_ok());
        let real_decode = decode.unwrap();
        assert_eq!(real_decode.version, 70002);
        assert_eq!(real_decode.locator_hashes.len(), 1);
        assert_eq!(serialize(&real_decode.locator_hashes[0]), genhash);
        assert_eq!(real_decode.stop_hash, Hash::all_zeros());

        assert_eq!(serialize(&real_decode), from_sat);
    }

    #[test]
    fn getheaders_message_test() {
        let from_sat = hex!(
            "72110100014a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b0000000000000000000000000000000000000000000000000000000000000000"
        );
        let genhash = hex!("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b");

        let decode: Result<GetHeadersMessage, _> = deserialize(&from_sat);
        assert!(decode.is_ok());
        let real_decode = decode.unwrap();
        assert_eq!(real_decode.version, 70002);
        assert_eq!(real_decode.locator_hashes.len(), 1);
        assert_eq!(serialize(&real_decode.locator_hashes[0]), genhash);
        assert_eq!(real_decode.stop_hash, Hash::all_zeros());

        assert_eq!(serialize(&real_decode), from_sat);
    }

    #[test]
    fn asset_unlock_inventory_round_trip() {
        let hash = crate::Txid::from_byte_array([0xab; 32]);
        let bytes = serialize(&Inventory::AssetUnlock(hash));
        assert_eq!(&bytes[..4], &34u32.to_le_bytes());
        assert_eq!(&bytes[4..], &[0xab; 32]);
        assert_eq!(deserialize::<Inventory>(&bytes).unwrap(), Inventory::AssetUnlock(hash));
    }

    fn inv_bytes(inv_type: u32, hash: [u8; 32]) -> Vec<u8> {
        let mut bytes = inv_type.to_le_bytes().to_vec();
        bytes.extend_from_slice(&hash);
        bytes
    }

    /// Dash Core's numbers: `MSG_CMPCT_BLOCK` is 20 (Bitcoin's 4 is the legacy
    /// `MSG_TXLOCK_REQUEST`) and every live type has its number.
    #[test]
    fn inventory_numbers_are_cores() {
        let h = [7u8; 32];
        let cases: [(u32, Inventory); 19] = [
            (1, Inventory::Transaction(Hash::from_byte_array(h))),
            (2, Inventory::Block(Hash::from_byte_array(h))),
            (3, Inventory::FilteredBlock(Hash::from_byte_array(h))),
            (6, Inventory::Spork(Hash::from_byte_array(h))),
            (16, Inventory::Dstx(Hash::from_byte_array(h))),
            (17, Inventory::GovernanceObject(Hash::from_byte_array(h))),
            (18, Inventory::GovernanceObjectVote(Hash::from_byte_array(h))),
            (20, Inventory::CompactBlock(Hash::from_byte_array(h))),
            (21, Inventory::QuorumFinalCommitment(Hash::from_byte_array(h))),
            (23, Inventory::QuorumContrib(Hash::from_byte_array(h))),
            (24, Inventory::QuorumComplaint(Hash::from_byte_array(h))),
            (25, Inventory::QuorumJustification(Hash::from_byte_array(h))),
            (26, Inventory::QuorumPrematureCommitment(Hash::from_byte_array(h))),
            (28, Inventory::QuorumRecoveredSig(Hash::from_byte_array(h))),
            (29, Inventory::ChainLock(Hash::from_byte_array(h))),
            (31, Inventory::InstantSendLock(Hash::from_byte_array(h))),
            (32, Inventory::Dsq(Hash::from_byte_array(h))),
            (33, Inventory::PlatformBan(Hash::from_byte_array(h))),
            (34, Inventory::AssetUnlock(Hash::from_byte_array(h))),
        ];
        for (number, inv) in cases {
            let bytes = inv_bytes(number, h);
            assert_eq!(inv.inv_type(), number);
            assert_eq!(serialize(&inv), bytes);
            assert_eq!(deserialize::<Inventory>(&bytes).unwrap(), inv);
            assert!(inv.is_known_type());
        }
    }

    /// Core skips what its `GetDataMsg` does not list: `UNDEFINED`, the legacy numbers, and
    /// BIP144's witness flag. They decode to `Unknown` and keep their bytes.
    #[test]
    fn unlisted_inventory_numbers_are_unknown() {
        let h = [9u8; 32];
        let numbers = [
            0u32,
            4,
            5,
            7,
            15,
            19,
            22,
            27,
            30,
            35,
            0x4000_0001,
            0x4000_0002,
            0x8000_0001,
            u32::MAX,
        ];
        for number in numbers {
            let bytes = inv_bytes(number, h);
            let inv = deserialize::<Inventory>(&bytes).unwrap();
            assert_eq!(
                inv,
                Inventory::Unknown {
                    inv_type: number,
                    hash: h
                }
            );
            assert!(!inv.is_known_type());
            assert_eq!(inv.command(), None);
            assert_eq!(serialize(&inv), bytes);
        }
        // Type 0 reads its hash too (`CInv` always reads both).
        assert!(deserialize::<Inventory>(&0u32.to_le_bytes()).is_err());
    }

    /// `CInv::ToString`: the command for a known type, `0x%08x` for an unknown one, and the hash
    /// byte-reversed.
    #[test]
    fn inventory_display_is_cores() {
        let mut h = [0u8; 32];
        h[0] = 0x01;
        h[31] = 0xfe;
        let rev = format!("fe{}01", "00".repeat(30));
        assert_eq!(
            Inventory::Transaction(Hash::from_byte_array(h)).to_string(),
            format!("tx {rev}")
        );
        assert_eq!(
            Inventory::AssetUnlock(Hash::from_byte_array(h)).to_string(),
            format!("tx {rev}")
        );
        assert_eq!(
            Inventory::CompactBlock(Hash::from_byte_array(h)).to_string(),
            format!("cmpctblock {rev}")
        );
        assert_eq!(
            Inventory::ChainLock(Hash::from_byte_array(h)).to_string(),
            format!("clsig {rev}")
        );
        assert_eq!(
            Inventory::Unknown {
                inv_type: 4,
                hash: h
            }
            .to_string(),
            format!("0x00000004 {rev}")
        );
        assert_eq!(
            Inventory::Unknown {
                inv_type: 0x4000_0001,
                hash: h
            }
            .to_string(),
            format!("0x40000001 {rev}")
        );
    }
}
