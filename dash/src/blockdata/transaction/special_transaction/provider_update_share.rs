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

//! Dash Provider Update Share Special Transaction.
//!
//! A provider update share transaction (ProUpShareTx) changes the reward script of one share of a
//! shared masternode. It is signed by the share's owner key over the payload hash.
//!
//! The special transaction type used for ProUpShareTx Transactions is 11 (Dash Core v24).

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};
use hashes::Hash;

use crate::blockdata::transaction::special_transaction::SpecialTransactionBasePayloadEncodable;
use crate::consensus::{Decodable, Encodable, encode};
use crate::hash_types::{InputsHash, SpecialTransactionPayloadHash, Txid};
use crate::prelude::*;
use crate::{ScriptBuf, VarInt, io};

/// A Provider Update Share Payload used in a Provider Update Share Special Transaction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ProviderUpdateSharePayload {
    pub version: u16,
    /// The registration transaction of the shared masternode.
    pub pro_tx_hash: Txid,
    /// The share whose reward script changes.
    pub share_index: u16,
    /// The new reward script; the share's refund script resets rewards to it.
    pub script_reward: ScriptBuf,
    pub inputs_hash: InputsHash,
    /// Compact signature by the share's owner key over [`Self::base_payload_hash`].
    pub payload_sig: Vec<u8>,
}

impl ProviderUpdateSharePayload {
    /// Latest spec version of the ProUpShareTx payload.
    pub const CURRENT_VERSION: u16 = 1;

    /// The size of the payload in bytes.
    pub fn size(&self) -> usize {
        2 + 32
            + 2
            + VarInt(self.script_reward.len() as u64).len()
            + self.script_reward.len()
            + 32
            + VarInt(self.payload_sig.len() as u64).len()
            + self.payload_sig.len()
    }
}

impl SpecialTransactionBasePayloadEncodable for ProviderUpdateSharePayload {
    fn base_payload_data_encode<W: io::Write>(&self, mut s: W) -> Result<usize, io::Error> {
        let mut len = 0;
        len += self.version.consensus_encode(&mut s)?;
        len += self.pro_tx_hash.consensus_encode(&mut s)?;
        len += self.share_index.consensus_encode(&mut s)?;
        len += self.script_reward.consensus_encode(&mut s)?;
        len += self.inputs_hash.consensus_encode(&mut s)?;
        Ok(len)
    }

    fn base_payload_hash(&self) -> SpecialTransactionPayloadHash {
        let mut engine = SpecialTransactionPayloadHash::engine();
        self.base_payload_data_encode(&mut engine).expect("engines don't error");
        SpecialTransactionPayloadHash::from_engine(engine)
    }
}

impl Encodable for ProviderUpdateSharePayload {
    fn consensus_encode<W: io::Write + ?Sized>(&self, mut w: &mut W) -> Result<usize, io::Error> {
        let mut len = self.base_payload_data_encode(&mut w)?;
        len += self.payload_sig.consensus_encode(&mut w)?;
        Ok(len)
    }
}

impl Decodable for ProviderUpdateSharePayload {
    fn consensus_decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, encode::Error> {
        Ok(ProviderUpdateSharePayload {
            version: u16::consensus_decode(r)?,
            pro_tx_hash: Txid::consensus_decode(r)?,
            share_index: u16::consensus_decode(r)?,
            script_reward: ScriptBuf::consensus_decode(r)?,
            inputs_hash: InputsHash::consensus_decode(r)?,
            payload_sig: Vec::<u8>::consensus_decode(r)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{deserialize, serialize};

    #[test]
    fn round_trip_layout() {
        let payload = ProviderUpdateSharePayload {
            version: 1,
            pro_tx_hash: Txid::from_byte_array([7; 32]),
            share_index: 3,
            script_reward: ScriptBuf::from_bytes(vec![0x51]),
            inputs_hash: InputsHash::from_byte_array([9; 32]),
            payload_sig: vec![0x1f; 65],
        };
        let bytes = serialize(&payload);
        assert_eq!(bytes.len(), payload.size());
        assert_eq!(&bytes[34..36], &[3, 0]);
        assert_eq!(&bytes[36..38], &[1, 0x51]);
        // The signature is length-prefixed and excluded from the payload hash
        assert_eq!(bytes[70], 65);
        let mut base = Vec::new();
        payload.base_payload_data_encode(&mut base).unwrap();
        assert_eq!(base, bytes[..70]);
        assert_eq!(deserialize::<ProviderUpdateSharePayload>(&bytes).unwrap(), payload);
    }
}
