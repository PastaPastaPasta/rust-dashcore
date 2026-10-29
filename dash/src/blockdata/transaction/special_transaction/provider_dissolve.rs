// Rust Dash Library
// Written for Dash in 2026 by
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

//! Dash Provider Dissolve Special Transaction.
//!
//! A provider dissolve transaction (ProDisTx) dissolves a shared masternode: it spends the shared
//! collateral and refunds every share. It carries one signature for a unilateral dissolution by
//! the share at `actor_index`, or one per share for a unanimous dissolution.
//!
//! The special transaction type used for ProDisTx Transactions is 10 (Dash Core v24).

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};

use crate::blockdata::transaction::special_transaction::provider_registration::{
    CompactSignature, decode_u8_list, encode_u8_list,
};
use crate::consensus::{Decodable, Encodable, encode};
use crate::hash_types::Txid;
use crate::io;
use crate::prelude::*;

/// A Provider Dissolve Payload used in a Provider Dissolve Special Transaction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ProviderDissolvePayload {
    pub version: u16,
    /// The registration transaction of the shared masternode.
    pub pro_tx_hash: Txid,
    /// The share that initiates a unilateral dissolution.
    pub actor_index: u16,
    /// One signature (unilateral) or one per share in share order (unanimous).
    pub signatures: Vec<CompactSignature>,
}

impl ProviderDissolvePayload {
    /// Latest spec version of the ProDisTx payload.
    pub const CURRENT_VERSION: u16 = 1;

    /// The size of the payload in bytes.
    pub fn size(&self) -> usize {
        2 + 32 + 2 + 1 + 65 * self.signatures.len()
    }
}

impl Encodable for ProviderDissolvePayload {
    fn consensus_encode<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<usize, io::Error> {
        let mut len = 0;
        len += self.version.consensus_encode(w)?;
        len += self.pro_tx_hash.consensus_encode(w)?;
        len += self.actor_index.consensus_encode(w)?;
        len += encode_u8_list(&self.signatures, w)?;
        Ok(len)
    }
}

impl Decodable for ProviderDissolvePayload {
    fn consensus_decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, encode::Error> {
        Ok(ProviderDissolvePayload {
            version: u16::consensus_decode(r)?,
            pro_tx_hash: Txid::consensus_decode(r)?,
            actor_index: u16::consensus_decode(r)?,
            signatures: decode_u8_list(r)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use hashes::Hash;

    use super::*;
    use crate::consensus::{deserialize, serialize};

    #[test]
    fn round_trip_layout() {
        let payload = ProviderDissolvePayload {
            version: 1,
            pro_tx_hash: Txid::from_byte_array([7; 32]),
            actor_index: 2,
            signatures: vec![CompactSignature([0x1f; 65]), CompactSignature([0x20; 65])],
        };
        let bytes = serialize(&payload);
        assert_eq!(bytes.len(), payload.size());
        assert_eq!(&bytes[..2], &[1, 0]);
        assert_eq!(&bytes[34..36], &[2, 0]);
        // u8 signature count, then raw 65-byte signatures without length prefixes
        assert_eq!(bytes[36], 2);
        assert_eq!(&bytes[37..102], &[0x1f; 65]);
        assert_eq!(deserialize::<ProviderDissolvePayload>(&bytes).unwrap(), payload);
    }
}
