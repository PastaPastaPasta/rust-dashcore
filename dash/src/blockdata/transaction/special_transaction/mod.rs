// Rust Dash Library
// Written for Dash in 2022 by
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

//! Dash Special Transaction.
//!
//! A dash special transaction's purpose is to relay more data than just economic information.
//! They are defined in DIP2 [dip-0002](https://github.com/dashpay/dips/blob/master/dip-0002.md).
//! The list of special transactions can be found here:
//! [dip-0002-special-transactions](https://github.com/dashpay/dips/blob/master/dip-0002-special-transactions.md)
//!

use core::convert::TryFrom;
use core::fmt::{Debug, Display, Formatter};

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};

use crate::blockdata::transaction::special_transaction::TransactionPayload::{
    AssetLockPayloadType, AssetUnlockPayloadType,
    ClassicalWithNonStandardVersionTypeBytesPayloadType, CoinbasePayloadType,
    MnhfSignalPayloadType, ProviderDissolvePayloadType, ProviderRegistrationPayloadType,
    ProviderUpdateRegistrarPayloadType, ProviderUpdateRevocationPayloadType,
    ProviderUpdateServicePayloadType, ProviderUpdateSharePayloadType,
    ProviderUpdateSharedRegistrarPayloadType, QuorumCommitmentPayloadType, UnknownPayloadType,
};
use crate::blockdata::transaction::special_transaction::TransactionType::{
    AssetLock, AssetUnlock, Classic, ClassicalWithNonStandardVersionTypeBytes, Coinbase,
    MnhfSignal, ProviderDissolve, ProviderRegistration, ProviderUpdateRegistrar,
    ProviderUpdateRevocation, ProviderUpdateService, ProviderUpdateShare,
    ProviderUpdateSharedRegistrar, QuorumCommitment, Unknown,
};
use crate::blockdata::transaction::special_transaction::asset_lock::AssetLockPayload;
use crate::blockdata::transaction::special_transaction::asset_unlock::qualified_asset_unlock::AssetUnlockPayload;
use crate::blockdata::transaction::special_transaction::coinbase::CoinbasePayload;
use crate::blockdata::transaction::special_transaction::mnhf_signal::MnhfSignalPayload;
use crate::blockdata::transaction::special_transaction::provider_dissolve::ProviderDissolvePayload;
use crate::blockdata::transaction::special_transaction::provider_registration::ProviderRegistrationPayload;
use crate::blockdata::transaction::special_transaction::provider_update_registrar::ProviderUpdateRegistrarPayload;
use crate::blockdata::transaction::special_transaction::provider_update_revocation::ProviderUpdateRevocationPayload;
use crate::blockdata::transaction::special_transaction::provider_update_service::ProviderUpdateServicePayload;
use crate::blockdata::transaction::special_transaction::provider_update_share::ProviderUpdateSharePayload;
use crate::blockdata::transaction::special_transaction::provider_update_shared_registrar::ProviderUpdateSharedRegistrarPayload;
use crate::blockdata::transaction::special_transaction::quorum_commitment::QuorumCommitmentPayload;
use crate::consensus::encode::VarInt;
use crate::consensus::{Decodable, Encodable, encode};
use crate::hash_types::SpecialTransactionPayloadHash;
use crate::io;
use crate::prelude::*;

pub mod asset_lock;
pub mod asset_unlock;
pub mod coinbase;
pub mod mnhf_signal;
pub mod provider_dissolve;
pub mod provider_registration;
pub mod provider_update_registrar;
pub mod provider_update_revocation;
pub mod provider_update_service;
pub mod provider_update_share;
pub mod provider_update_shared_registrar;
pub mod quorum_commitment;

/// An enum wrapper around various special transaction payloads.
/// Special transactions are defined in DIP 2.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum TransactionPayload {
    /// A wrapper for a Masternode Registration payload
    ProviderRegistrationPayloadType(ProviderRegistrationPayload),
    /// A wrapper for a Masternode Update Service payload
    ProviderUpdateServicePayloadType(ProviderUpdateServicePayload),
    /// A wrapper for a Masternode Update Registrar payload
    ProviderUpdateRegistrarPayloadType(ProviderUpdateRegistrarPayload),
    /// A wrapper for a Masternode Update Revocation payload
    ProviderUpdateRevocationPayloadType(ProviderUpdateRevocationPayload),
    /// A wrapper for a Coinbase payload
    CoinbasePayloadType(CoinbasePayload),
    /// A wrapper for a Quorum Commitment payload
    QuorumCommitmentPayloadType(QuorumCommitmentPayload),
    /// A wrapper for a MNHF Signal payload
    MnhfSignalPayloadType(MnhfSignalPayload),
    /// A wrapper for an Asset Lock payload
    AssetLockPayloadType(AssetLockPayload),
    /// A wrapper for an Asset Unlock payload
    AssetUnlockPayloadType(AssetUnlockPayload),
    /// A pseudo-payload that carries the raw `nTxType` u16 read from a pre-DIP-0002
    /// transaction (`version == 0`). Older transactions on the chain were free to put
    /// arbitrary bytes in the type slot; we keep the original value here so that
    /// `consensus_encode` and `txid` continue to round-trip the on-wire bytes faithfully.
    /// This variant has no payload section on the wire.
    ClassicalWithNonStandardVersionTypeBytesPayloadType(u16),
    /// The raw payload of a special transaction this crate cannot interpret, together with
    /// its `nTxType`: either a type this crate does not know, or a known type whose payload
    /// does not decode exactly (for example a newer payload version). Keeping the bytes lets
    /// blocks containing such transactions decode, and re-encode to the same bytes and txid.
    UnknownPayloadType(u16, Vec<u8>),
    /// A wrapper for a Provider Dissolve payload (Dash Core v24)
    ProviderDissolvePayloadType(ProviderDissolvePayload),
    /// A wrapper for a Provider Update Share payload (Dash Core v24)
    ProviderUpdateSharePayloadType(ProviderUpdateSharePayload),
    /// A wrapper for a Provider Update Shared Registrar payload (Dash Core v24)
    ProviderUpdateSharedRegistrarPayloadType(ProviderUpdateSharedRegistrarPayload),
}

impl Encodable for TransactionPayload {
    fn consensus_encode<W: io::Write + ?Sized>(&self, w: &mut W) -> Result<usize, io::Error> {
        match self {
            ProviderRegistrationPayloadType(p) => p.consensus_encode(w),
            ProviderUpdateServicePayloadType(p) => p.consensus_encode(w),
            ProviderUpdateRegistrarPayloadType(p) => p.consensus_encode(w),
            ProviderUpdateRevocationPayloadType(p) => p.consensus_encode(w),
            CoinbasePayloadType(p) => p.consensus_encode(w),
            QuorumCommitmentPayloadType(p) => p.consensus_encode(w),
            MnhfSignalPayloadType(p) => p.consensus_encode(w),
            AssetLockPayloadType(p) => p.consensus_encode(w),
            AssetUnlockPayloadType(p) => p.consensus_encode(w),
            ProviderDissolvePayloadType(p) => p.consensus_encode(w),
            ProviderUpdateSharePayloadType(p) => p.consensus_encode(w),
            ProviderUpdateSharedRegistrarPayloadType(p) => p.consensus_encode(w),
            ClassicalWithNonStandardVersionTypeBytesPayloadType(_) => Ok(0),
            UnknownPayloadType(_, bytes) => {
                w.write_all(bytes)?;
                Ok(bytes.len())
            }
        }
    }
}

impl TransactionPayload {
    /// Gets the Transaction Type for a Special Transaction Payload
    pub fn get_type(&self) -> TransactionType {
        match self {
            ProviderRegistrationPayloadType(_) => ProviderRegistration,
            ProviderUpdateServicePayloadType(_) => ProviderUpdateService,
            ProviderUpdateRegistrarPayloadType(_) => ProviderUpdateRegistrar,
            ProviderUpdateRevocationPayloadType(_) => ProviderUpdateRevocation,
            CoinbasePayloadType(_) => Coinbase,
            QuorumCommitmentPayloadType(_) => QuorumCommitment,
            MnhfSignalPayloadType(_) => MnhfSignal,
            AssetLockPayloadType(_) => AssetLock,
            AssetUnlockPayloadType(_) => AssetUnlock,
            ProviderDissolvePayloadType(_) => ProviderDissolve,
            ProviderUpdateSharePayloadType(_) => ProviderUpdateShare,
            ProviderUpdateSharedRegistrarPayloadType(_) => ProviderUpdateSharedRegistrar,
            ClassicalWithNonStandardVersionTypeBytesPayloadType(raw) => {
                ClassicalWithNonStandardVersionTypeBytes(*raw)
            }
            UnknownPayloadType(raw, _) => Unknown(*raw),
        }
    }

    /// Gets the size of the payload section on the wire: the payload together with its
    /// compact size length prefix.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        match self {
            // Pre-DIP-0002 transactions have no payload section on the wire.
            ClassicalWithNonStandardVersionTypeBytesPayloadType(_) => 0,
            _ => {
                let size = self.size();
                VarInt(size as u64).len() + size
            }
        }
    }

    /// Gets the size of the encoded payload, without its length prefix.
    pub fn size(&self) -> usize {
        match self {
            ProviderRegistrationPayloadType(p) => p.size(),
            ProviderUpdateServicePayloadType(p) => p.size(),
            ProviderUpdateRegistrarPayloadType(p) => p.size(),
            ProviderUpdateRevocationPayloadType(p) => p.size(),
            CoinbasePayloadType(p) => p.size(),
            QuorumCommitmentPayloadType(p) => p.size(),
            MnhfSignalPayloadType(p) => p.size(),
            AssetLockPayloadType(p) => p.size(),
            AssetUnlockPayloadType(p) => p.size(),
            ProviderDissolvePayloadType(p) => p.size(),
            ProviderUpdateSharePayloadType(p) => p.size(),
            ProviderUpdateSharedRegistrarPayloadType(p) => p.size(),
            ClassicalWithNonStandardVersionTypeBytesPayloadType(_) => 0,
            UnknownPayloadType(_, bytes) => bytes.len(),
        }
    }

    /// Convenience method that assumes the payload to be a provider registration payload to get it
    /// easier.
    /// Errors if it is not a provider registration payload.
    pub fn to_provider_registration_payload(
        self,
    ) -> Result<ProviderRegistrationPayload, encode::Error> {
        if let ProviderRegistrationPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: ProviderRegistration,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a provider update service payload to get it
    /// easier.
    /// Errors if it is not a provider update service payload.
    pub fn to_update_service_payload(self) -> Result<ProviderUpdateServicePayload, encode::Error> {
        if let ProviderUpdateServicePayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: ProviderUpdateService,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a provider update registrar payload to get it
    /// easier.
    /// Errors if it is not a provider update registrar payload.
    pub fn to_update_registrar_payload(
        self,
    ) -> Result<ProviderUpdateRegistrarPayload, encode::Error> {
        if let ProviderUpdateRegistrarPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: ProviderUpdateRegistrar,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a provider update revocation payload to get it
    /// easier.
    /// Errors if it is not a provider update revocation payload.
    pub fn to_update_revocation_payload(
        self,
    ) -> Result<ProviderUpdateRevocationPayload, encode::Error> {
        if let ProviderUpdateRevocationPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: ProviderUpdateRevocation,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a coinbase payload to get it
    /// easier.
    /// Errors if it is not a coinbase payload.
    pub fn to_coinbase_payload(self) -> Result<CoinbasePayload, encode::Error> {
        if let CoinbasePayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: Coinbase,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a quorum commitment payload to get it
    /// easier.
    /// Errors if it is not a quorum commitment payload.
    pub fn to_quorum_commitment_payload(self) -> Result<QuorumCommitmentPayload, encode::Error> {
        if let QuorumCommitmentPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: QuorumCommitment,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be an asset lock payload to get it
    /// easier.
    /// Errors if it is not an asset lock payload.
    pub fn to_asset_lock_payload(self) -> Result<AssetLockPayload, encode::Error> {
        if let AssetLockPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: AssetLock,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a credit withdrawal payload to get it
    /// easier.
    /// Errors if it is not a credit withdrawal payload.
    pub fn to_asset_unlock_payload(self) -> Result<AssetUnlockPayload, encode::Error> {
        if let AssetUnlockPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: AssetLock,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a provider dissolve payload to get it
    /// easier.
    /// Errors if it is not a provider dissolve payload.
    pub fn to_provider_dissolve_payload(self) -> Result<ProviderDissolvePayload, encode::Error> {
        if let ProviderDissolvePayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: ProviderDissolve,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a provider update share payload to get
    /// it easier.
    /// Errors if it is not a provider update share payload.
    pub fn to_update_share_payload(self) -> Result<ProviderUpdateSharePayload, encode::Error> {
        if let ProviderUpdateSharePayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: ProviderUpdateShare,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a provider update shared registrar
    /// payload to get it easier.
    /// Errors if it is not a provider update shared registrar payload.
    pub fn to_update_shared_registrar_payload(
        self,
    ) -> Result<ProviderUpdateSharedRegistrarPayload, encode::Error> {
        if let ProviderUpdateSharedRegistrarPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: ProviderUpdateSharedRegistrar,
                actual: self.get_type(),
            })
        }
    }

    /// Convenience method that assumes the payload to be a MNHF signal payload to get it
    /// easier.
    /// Errors if it is not a MNHF signal payload.
    pub fn to_mnhf_signal_payload(self) -> Result<MnhfSignalPayload, encode::Error> {
        if let MnhfSignalPayloadType(payload) = self {
            Ok(payload)
        } else {
            Err(encode::Error::WrongSpecialTransactionPayloadConversion {
                expected: MnhfSignal,
                actual: self.get_type(),
            })
        }
    }
}

/// The transaction type. Special transactions were introduced in DIP2.
/// Compared to Bitcoin the version field is split into two 16-bit integers.
/// The first part for the version and the second part for the transaction
/// type.
///
/// Pre-DIP-0002 transactions on Dash mainnet (`version == 0`) were free to put
/// arbitrary bytes in the type slot. The [`ClassicalWithNonStandardVersionTypeBytes`]
/// variant preserves the raw u16 read from the wire so that those transactions can
/// still round-trip through `consensus_encode` / `txid` without altering the on-chain
/// bytes or hashes. Logically these transactions behave as Classic.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransactionType {
    /// A Classic transaction
    Classic,
    /// A Masternode Registration Transaction
    ProviderRegistration,
    /// A Masternode Update Service Transaction, used by the operator to signal changes to service
    ProviderUpdateService,
    /// A Masternode Update Registrar Transaction, used by the owner to signal base changes
    ProviderUpdateRegistrar,
    /// A Masternode Update Revocation Transaction, used by the operator to signal termination of service
    ProviderUpdateRevocation,
    /// A Coinbase Transaction, contained as the first transaction in each block
    Coinbase,
    /// A Quorum Commitment Transaction, used to save quorum information to the state
    QuorumCommitment,
    /// A MNHF Signal Transaction, used by masternodes to signal consensus for hard fork activations
    MnhfSignal,
    /// An Asset Lock Transaction, used to transfer credits to Dash Platform, by locking them until withdrawals occur
    AssetLock,
    /// An Asset Unlock Transaction, used to withdraw credits from Dash Platform, by unlocking them
    AssetUnlock,
    /// A transaction whose version is not special (below 3) but whose on-wire `nTxType`
    /// bytes are non-zero; it has no payload. The wrapped value is the original u16 read from
    /// the wire, which must be re-emitted verbatim during serialization to preserve the txid.
    ClassicalWithNonStandardVersionTypeBytes(u16),
    /// A special transaction whose payload this crate cannot interpret: an unknown type, or a
    /// known type whose payload does not decode exactly. The wrapped value is the on-wire
    /// `nTxType`; the payload is kept as raw bytes.
    Unknown(u16),
    /// A Provider Dissolve Transaction, which dissolves a shared masternode (Dash Core v24)
    ProviderDissolve,
    /// A Provider Update Share Transaction, which changes the reward script of one share of a
    /// shared masternode (Dash Core v24)
    ProviderUpdateShare,
    /// A Provider Update Shared Registrar Transaction, which changes the operator and voting keys
    /// of a shared masternode (Dash Core v24)
    ProviderUpdateSharedRegistrar,
}

impl Debug for TransactionType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match *self {
            Classic => write!(f, "Classic Transaction"),
            ProviderRegistration => write!(f, "Provider Registration Transaction"),
            ProviderUpdateService => write!(f, "Provider Update Service Transaction"),
            ProviderUpdateRegistrar => write!(f, "Provider Update Registrar Transaction"),
            ProviderUpdateRevocation => write!(f, "Provider Update Revocation Transaction"),
            Coinbase => write!(f, "Coinbase Transaction"),
            QuorumCommitment => write!(f, "Quorum Commitment Transaction"),
            MnhfSignal => write!(f, "MNHF Signal Transaction"),
            AssetLock => write!(f, "Asset Lock Transaction"),
            AssetUnlock => write!(f, "Asset Unlock Transaction"),
            ProviderDissolve => write!(f, "Provider Dissolve Transaction"),
            ProviderUpdateShare => write!(f, "Provider Update Share Transaction"),
            ProviderUpdateSharedRegistrar => {
                write!(f, "Provider Update Shared Registrar Transaction")
            }
            ClassicalWithNonStandardVersionTypeBytes(raw) => {
                write!(f, "Classic Transaction (non-special version, raw type bytes 0x{raw:04x})")
            }
            Unknown(raw) => write!(f, "Unknown Special Transaction (type {raw})"),
        }
    }
}

impl Display for TransactionType {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match *self {
            Classic => write!(f, "Classic"),
            ProviderRegistration => write!(f, "Provider Registration"),
            ProviderUpdateService => write!(f, "Provider Update Service"),
            ProviderUpdateRegistrar => write!(f, "Provider Update Registrar"),
            ProviderUpdateRevocation => write!(f, "Provider Update Revocation"),
            Coinbase => write!(f, "Coinbase"),
            QuorumCommitment => write!(f, "Quorum Commitment"),
            MnhfSignal => write!(f, "MNHF Signal"),
            AssetLock => write!(f, "Asset Lock"),
            AssetUnlock => write!(f, "Asset Unlock"),
            ProviderDissolve => write!(f, "Provider Dissolve"),
            ProviderUpdateShare => write!(f, "Provider Update Share"),
            ProviderUpdateSharedRegistrar => write!(f, "Provider Update Shared Registrar"),
            ClassicalWithNonStandardVersionTypeBytes(raw) => {
                write!(f, "Classic (non-special version, raw 0x{raw:04x})")
            }
            Unknown(raw) => write!(f, "Unknown ({raw})"),
        }
    }
}

impl TryFrom<u16> for TransactionType {
    type Error = encode::Error;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Classic),
            1 => Ok(ProviderRegistration),
            2 => Ok(ProviderUpdateService),
            3 => Ok(ProviderUpdateRegistrar),
            4 => Ok(ProviderUpdateRevocation),
            5 => Ok(Coinbase),
            6 => Ok(QuorumCommitment),
            7 => Ok(MnhfSignal),
            8 => Ok(AssetLock),
            9 => Ok(AssetUnlock),
            10 => Ok(ProviderDissolve),
            11 => Ok(ProviderUpdateShare),
            12 => Ok(ProviderUpdateSharedRegistrar),
            _ => Err(encode::Error::UnknownSpecialTransactionType(value)),
        }
    }
}

impl Decodable for TransactionType {
    fn consensus_decode<R: io::Read + ?Sized>(r: &mut R) -> Result<Self, encode::Error> {
        let special_transaction_number = u16::consensus_decode(r)?;
        TransactionType::try_from(special_transaction_number)
    }
}

impl TransactionType {
    /// Get the transaction type from an optional payload
    /// If the payload in None then we have a Classical Transaction
    pub fn from_optional_payload(payload: &Option<TransactionPayload>) -> Self {
        match payload {
            None => Classic,
            Some(payload) => payload.get_type(),
        }
    }

    /// Returns the on-wire `u16` representation of the transaction type.
    ///
    /// For pre-DIP-0002 [`ClassicalWithNonStandardVersionTypeBytes`] this returns the
    /// original raw bytes that were read from the chain so that re-encoding/hashing the
    /// transaction reproduces the on-chain bytes verbatim.
    pub fn to_u16(self) -> u16 {
        match self {
            Classic => 0,
            ProviderRegistration => 1,
            ProviderUpdateService => 2,
            ProviderUpdateRegistrar => 3,
            ProviderUpdateRevocation => 4,
            Coinbase => 5,
            QuorumCommitment => 6,
            MnhfSignal => 7,
            AssetLock => 8,
            AssetUnlock => 9,
            ProviderDissolve => 10,
            ProviderUpdateShare => 11,
            ProviderUpdateSharedRegistrar => 12,
            ClassicalWithNonStandardVersionTypeBytes(raw) => raw,
            Unknown(raw) => raw,
        }
    }

    /// Decodes the payload section of a transaction of this type.
    ///
    /// Like Core's `vExtraPayload`, the payload of a special transaction is read through its
    /// length prefix, so the stream stays aligned whatever the payload holds. It is then parsed
    /// as the typed payload of this type, which must consume it exactly and re-encode to the
    /// same bytes; otherwise (unknown type, newer payload version, non-canonical encoding) the
    /// raw bytes are kept as [`TransactionPayload::UnknownPayloadType`] so that the transaction
    /// still re-encodes to the same bytes and txid.
    ///
    /// A typed payload is not a valid one, and the typed decoding differs from Core's payload
    /// extraction (`GetTxPayload`) for payloads Core then refuses: Core extracts a ProTx payload of
    /// an unknown version holding nothing but the version (refused as `bad-protx-version`) and a
    /// ProRegTx with a masternode type other than 0 and 1, which stay raw here; and Core fails the
    /// extraction of a BLS signature or key it reads eagerly (signatures of ProUpServTx,
    /// ProUpRevTx and MNHF signals, `bestCLSignature`, `quorumSig`, quorum commitments) unless the
    /// bytes are all zero or a canonical encoding, which this crate doesn't check.
    pub fn consensus_decode<R: io::Read + ?Sized>(
        self,
        d: &mut R,
    ) -> Result<Option<TransactionPayload>, encode::Error> {
        // Classic and non-special-version transactions have no payload section on the wire,
        // there isn't even a length prefix to consume.
        let raw = match self {
            Classic => return Ok(None),
            ClassicalWithNonStandardVersionTypeBytes(raw) => {
                return Ok(Some(ClassicalWithNonStandardVersionTypeBytesPayloadType(raw)));
            }
            _ => Vec::<u8>::consensus_decode(d)?,
        };
        let typed = self.decode_typed_payload(&raw).ok().filter(|payload| {
            let mut encoded = Vec::with_capacity(raw.len());
            payload.consensus_encode(&mut encoded).is_ok() && encoded == raw
        });
        Ok(Some(typed.unwrap_or_else(|| UnknownPayloadType(self.to_u16(), raw))))
    }

    /// Parses `raw` as the typed payload of this type, requiring every byte to be consumed.
    fn decode_typed_payload(self, raw: &[u8]) -> Result<TransactionPayload, encode::Error> {
        let d = &mut &raw[..];
        let payload = match self {
            ProviderRegistration => {
                ProviderRegistrationPayloadType(ProviderRegistrationPayload::consensus_decode(d)?)
            }
            ProviderUpdateService => {
                ProviderUpdateServicePayloadType(ProviderUpdateServicePayload::consensus_decode(d)?)
            }
            ProviderUpdateRegistrar => ProviderUpdateRegistrarPayloadType(
                ProviderUpdateRegistrarPayload::consensus_decode(d)?,
            ),
            ProviderUpdateRevocation => ProviderUpdateRevocationPayloadType(
                ProviderUpdateRevocationPayload::consensus_decode(d)?,
            ),
            Coinbase => CoinbasePayloadType(CoinbasePayload::consensus_decode(d)?),
            QuorumCommitment => {
                QuorumCommitmentPayloadType(QuorumCommitmentPayload::consensus_decode(d)?)
            }
            MnhfSignal => MnhfSignalPayloadType(MnhfSignalPayload::consensus_decode(d)?),
            AssetLock => AssetLockPayloadType(AssetLockPayload::consensus_decode(d)?),
            AssetUnlock => AssetUnlockPayloadType(AssetUnlockPayload::consensus_decode(d)?),
            ProviderDissolve => {
                ProviderDissolvePayloadType(ProviderDissolvePayload::consensus_decode(d)?)
            }
            ProviderUpdateShare => {
                ProviderUpdateSharePayloadType(ProviderUpdateSharePayload::consensus_decode(d)?)
            }
            ProviderUpdateSharedRegistrar => ProviderUpdateSharedRegistrarPayloadType(
                ProviderUpdateSharedRegistrarPayload::consensus_decode(d)?,
            ),
            Classic | ClassicalWithNonStandardVersionTypeBytes(_) | Unknown(_) => {
                return Err(encode::Error::ParseFailed("no typed payload for this type"));
            }
        };
        if !d.is_empty() {
            return Err(encode::Error::ParseFailed(
                "special transaction payload has trailing data",
            ));
        }
        Ok(payload)
    }
}

/// Data which can be encoded in a consensus-consistent way
pub trait SpecialTransactionBasePayloadEncodable {
    /// Encode the payload with a well-defined format.
    /// Returns the number of bytes written on success.
    ///
    /// Errors come from the writer, or from a payload built in code that can't be encoded (a
    /// version 3 ProTx without `net_info` or payouts, a share count that doesn't match the join
    /// signatures, ...).
    fn base_payload_data_encode<W: io::Write>(&self, writer: W) -> Result<usize, io::Error>;

    /// The hash of the base payload special transaction data.
    ///
    /// # Panics
    ///
    /// If the payload can't be encoded (see [`Self::base_payload_data_encode`]). Decoded
    /// payloads always encode.
    fn base_payload_hash(&self) -> SpecialTransactionPayloadHash;
}
