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

//! Dash Provider Update Shared Registrar Special Transaction.
//!
//! A provider update shared registrar transaction (ProUpSharedRegTx) changes the operator and
//! voting keys of a shared masternode. Every share owner signs the payload hash.
//!
//! The special transaction type used for ProUpSharedRegTx Transactions is 12 (Dash Core v24).

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};
use hashes::Hash;

use crate::blockdata::transaction::special_transaction::SpecialTransactionBasePayloadEncodable;
use crate::blockdata::transaction::special_transaction::provider_registration::{
    CompactSignature, decode_u8_list, encode_u8_list,
};
use crate::bls_sig_utils::BLSPublicKey;
use crate::consensus::{Decodable, Encodable, encode};
use crate::hash_types::{InputsHash, PubkeyHash, SpecialTransactionPayloadHash, Txid};
use crate::io;
use crate::prelude::*;

/// A Provider Update Shared Registrar Payload used in a Provider Update Shared Registrar Special
/// Transaction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ProviderUpdateSharedRegistrarPayload {
    pub version: u16,
    /// The registration transaction of the shared masternode.
    pub pro_tx_hash: Txid,
    /// The new operator key, always in the basic BLS encoding.
    pub operator_public_key: BLSPublicKey,
    pub voting_key_hash: PubkeyHash,
    pub inputs_hash: InputsHash,
    /// One signature per share in share order, over [`Self::base_payload_hash`].
    pub signatures: Vec<CompactSignature>,
}

impl ProviderUpdateSharedRegistrarPayload {
    /// Latest spec version of the ProUpSharedRegTx payload.
    pub const CURRENT_VERSION: u16 = 1;

    /// The size of the payload in bytes.
    pub fn size(&self) -> usize {
        2 + 32 + 48 + 20 + 32 + 1 + 65 * self.signatures.len()
    }
}

impl SpecialTransactionBasePayloadEncodable for ProviderUpdateSharedRegistrarPayload {
    fn base_payload_data_encode<W: io::Write>(&self, mut s: W) -> Result<usize, io::Error> {
        let mut len = 0;
        len += self.version.consensus_encode(&mut s)?;
        len += self.pro_tx_hash.consensus_encode(&mut s)?;
        len += self.operator_public_key.consensus_encode(&mut s)?;
        len += self.voting_key_hash.consensus_encode(&mut s)?;
        len += self.inputs_hash.consensus_encode(&mut s)?;
        Ok(len)
    }

    fn base_payload_hash(&self) -> SpecialTransactionPayloadHash {
        let mut engine = SpecialTransactionPayloadHash::engine();
        self.base_payload_data_encode(&mut engine).expect("engines don't error");
        SpecialTransactionPayloadHash::from_engine(engine)
    }
}

impl Encodable for ProviderUpdateSharedRegistrarPayload {
    fn consensus_encode<W: io::Write + ?Sized>(&self, mut w: &mut W) -> Result<usize, io::Error> {
        let mut len = self.base_payload_data_encode(&mut w)?;
        len += encode_u8_list(&self.signatures, w)?;
        Ok(len)
    }
}

impl Decodable for ProviderUpdateSharedRegistrarPayload {
    fn consensus_decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, encode::Error> {
        Ok(ProviderUpdateSharedRegistrarPayload {
            version: u16::consensus_decode(r)?,
            pro_tx_hash: Txid::consensus_decode(r)?,
            operator_public_key: BLSPublicKey::consensus_decode(r)?,
            voting_key_hash: PubkeyHash::consensus_decode(r)?,
            inputs_hash: InputsHash::consensus_decode(r)?,
            signatures: decode_u8_list(r)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{deserialize, serialize};

    #[test]
    fn round_trip_layout() {
        let payload = ProviderUpdateSharedRegistrarPayload {
            version: 1,
            pro_tx_hash: Txid::from_byte_array([7; 32]),
            operator_public_key: BLSPublicKey::from([3; 48]),
            voting_key_hash: PubkeyHash::from_byte_array([4; 20]),
            inputs_hash: InputsHash::from_byte_array([9; 32]),
            signatures: vec![CompactSignature([0x1f; 65]); 3],
        };
        let bytes = serialize(&payload);
        assert_eq!(bytes.len(), payload.size());
        // The u8 signature count and the signatures follow the hashed part
        let mut base = Vec::new();
        payload.base_payload_data_encode(&mut base).unwrap();
        assert_eq!(base, bytes[..134]);
        assert_eq!(bytes[134], 3);
        assert_eq!(deserialize::<ProviderUpdateSharedRegistrarPayload>(&bytes).unwrap(), payload);
    }
}
