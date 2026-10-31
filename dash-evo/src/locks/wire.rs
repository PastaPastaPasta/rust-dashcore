//! The lock messages' wire forms (LQ3a): Core's hashes of `clsig`, `isdlock` and `qsigrec`, and
//! how Core reads their signature bytes.
//!
//! **Quirk 38.** Core hashes a `clsig` over its re-serialization: the signature is read eagerly
//! and written back as Core holds it. It hashes an `isdlock` and a `qsigrec` over their wire
//! bytes: the signature is a lazy field whose raw bytes are what Core writes until it is first
//! used. The two rules differ for a ChainLock signature that is canonical in the other scheme
//! alone, which Core reads as the invalid object and writes back as zeros (quirk 37), and for one
//! canonical in neither scheme, which Core cannot read at all (no `clsig`, so no hash).

use dash_consensus::hash::HashWriter;
use dash_consensus::primitives::{CoreWrite, write_compact_size};
use dash_p2p_core::{ClSig, IsdLock};
use dash_pkc::bls::core::BlsSigValue;
use dash_pkc::bls::{BlsGlobalSig, BlsLazySig, BlsSchemeSource};
use dash_primitives::{LlmqType, QuorumHash};
use dash_types::Numeric;

use super::sign_hash;
use crate::bls::BlsCtx;

/// The length of a BLS signature on the wire.
pub const SIG_LEN: usize = 96;

/// How Core reads a lock's signature bytes under a scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "returned and matched at once: a box would allocate per message"
)]
pub enum SigRead {
    /// Core's reader throws, so the message is never read: the bytes are canonical in neither
    /// scheme (the identity encoding included). Only an eagerly read field ([`read_eager_sig`])
    /// gives this.
    Unreadable,
    /// The invalid object: zeros, or bytes canonical only in the other scheme. Core writes it back
    /// as zeros, and no key verifies it.
    Invalid,
    /// A signature valid under the scheme.
    Valid(BlsSigValue),
}

/// A `clsig`'s signature as Core's `ChainLockSig` reads it, eagerly under the process-wide
/// scheme (`CBLSSignature::Unserialize`).
// Core: chainlock/chainlock.h:41 ChainLockSig SERIALIZE_METHODS; bls/bls.h:184 CBLSWrapper::Unserialize
pub fn read_eager_sig(bytes: &[u8; SIG_LEN], bls: BlsCtx) -> SigRead {
    let Ok(field) = BlsGlobalSig::decode(bytes) else {
        return SigRead::Unreadable;
    };
    let eager = field.get(bls);
    if eager.is_valid() {
        SigRead::Valid(eager.into_value())
    } else {
        SigRead::Invalid
    }
}

/// An `isdlock`'s or a `qsigrec`'s signature as Core reads it on first use: a lazy field holds the
/// raw bytes and the scheme of the time it was read, and `Get()` decodes them under that scheme
/// alone. It never throws, so it is never [`SigRead::Unreadable`].
// Core: bls/bls.h:492 CBLSLazyWrapper::Get
pub fn read_lazy_sig(bytes: &[u8; SIG_LEN], bls: BlsCtx) -> SigRead {
    let value = BlsLazySig::from_bytes(bytes, BlsSchemeSource::Global(bls)).get();
    if value.is_valid() {
        SigRead::Valid(value)
    } else {
        SigRead::Invalid
    }
}

/// The two reads of the process-wide BLS flag behind a `clsig`'s inventory hash: `read` when the
/// message is deserialized, `write` when `SerializeHash` writes it back. Core holds no lock
/// across them (`net_processing.cpp` v24:5797-5798), so they differ when the V19 block connects
/// or disconnects in between (LQ3a D19). Named fields, so a caller cannot swap them by position;
/// `read` belongs to the message, as [`LockSchemes`](super::LockSchemes)'s does (LQ3a D26, D27).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClsigHashSchemes {
    /// The flag when the message was read.
    pub read: BlsCtx,
    /// The flag when the hash writes the signature back.
    pub write: BlsCtx,
}

impl ClsigHashSchemes {
    /// One scheme for the read and the write: no switch between them.
    pub const fn both(bls: BlsCtx) -> Self {
        ClsigHashSchemes {
            read: bls,
            write: bls,
        }
    }
}

/// Core's inventory hash of a `clsig` (`MSG_CLSIG`): `::SerializeHash(clsig)` of the message as
/// read, the signature read eagerly under `schemes.read` and written back under `schemes.write`
/// (quirk 38; [`ClsigHashSchemes`]). `None` where Core's reader throws ([`SigRead::Unreadable`]),
/// so Core has no hash. Across the V19 switch a legacy signature read before it is hashed in its
/// basic encoding (LQ3a D19).
// Core: net_processing.cpp:5798 (CLSIG), chainlock/chainlock.h:41 SERIALIZE_METHODS;
// bls/bls.h:170-209 CBLSWrapper::Serialize, ::Unserialize
pub fn clsig_hash(clsig: &ClSig, schemes: ClsigHashSchemes) -> Option<[u8; 32]> {
    let field = BlsGlobalSig::decode(clsig.sig.as_bytes()).ok()?;
    let written = BlsGlobalSig::from_eager(&field.get(schemes.read), schemes.write).to_bytes();
    let mut w = HashWriter::new();
    w.write(&clsig.height.to_le_bytes());
    w.write(clsig.block_hash.as_bytes());
    w.write(&written);
    Some(w.finish())
}

/// Core's inventory hash of an `isdlock` (`MSG_ISDLOCK`): `::SerializeHash(*islock)` after reading
/// it, before its signature is first used, so the lazy signature writes its raw bytes: the hash of
/// the lock's wire bytes (quirk 38). Core hashes only a lock of at most
/// [`ISLOCK_MAX_INPUTS`](super::ISLOCK_MAX_INPUTS) inputs (`net_instantsend.cpp` v24:252-258).
// Core: instantsend/net_instantsend.cpp:258, instantsend/lock.h:41 SERIALIZE_METHODS
pub fn isdlock_hash(islock: &IsdLock) -> [u8; 32] {
    let mut w = HashWriter::new();
    w.write(&[islock.version]);
    write_compact_size(islock.inputs.len() as u64, &mut w);
    for input in &islock.inputs {
        w.write(input.hash.as_bytes());
        w.write(&input.index.to_le_bytes());
    }
    w.write(islock.txid.as_bytes());
    w.write(islock.cycle_hash.as_bytes());
    w.write(islock.sig.as_bytes());
    w.finish()
}

/// Core's `CRecoveredSig`, the `qsigrec` payload: a quorum's recovered signature over
/// `SignHash(type, quorumHash, id, msgHash)`, with the signature's raw bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredSig {
    /// `llmqType`, any byte (Core reads the enum's underlying `uint8_t`).
    pub llmq_type: LlmqType,
    /// `quorumHash`: the quorum the signature names.
    pub quorum_hash: QuorumHash,
    /// `id`: the signing request id.
    pub id: [u8; 32],
    /// `msgHash`: the signed message.
    pub msg_hash: [u8; 32],
    /// `sig`, a lazy field: the bytes as received.
    pub sig: [u8; SIG_LEN],
}

impl RecoveredSig {
    /// The length of the serialization.
    pub const LEN: usize = 1 + 32 + 32 + 32 + SIG_LEN;

    /// Core's `vRecv >> recoveredSig`: reads one from the front of `r` and advances it. `None`
    /// when fewer than [`Self::LEN`] bytes remain, where Core's read throws. Core reads the
    /// message's object and does not look at what follows it.
    // Core: llmq/signing.h:96 SERIALIZE_METHODS; llmq/net_signing.cpp:51 (QSIGREC)
    pub fn decode(r: &mut &[u8]) -> Option<RecoveredSig> {
        let (bytes, rest) = r.split_first_chunk::<{ RecoveredSig::LEN }>()?;
        *r = rest;
        let field = |at: usize| -> [u8; 32] { bytes[at..at + 32].try_into().expect("32 bytes") };
        Some(RecoveredSig {
            llmq_type: LlmqType::from_base(bytes[0]),
            quorum_hash: QuorumHash::from(field(1)),
            id: field(33),
            msg_hash: field(65),
            sig: bytes[97..].try_into().expect("a signature"),
        })
    }

    /// Core's `GetHash()` (`MSG_QUORUM_RECOVERED_SIG`): `::SerializeHash(*this)` taken when the
    /// message is read, so over the raw signature bytes, the wire bytes (quirk 38).
    // Core: llmq/signing.h:90-100 CRecoveredSig::UpdateHash
    pub fn hash(&self) -> [u8; 32] {
        let mut w = HashWriter::new();
        w.write(&[self.llmq_type.to_base()]);
        w.write(self.quorum_hash.as_bytes());
        w.write(&self.id);
        w.write(&self.msg_hash);
        w.write(&self.sig);
        w.finish()
    }

    /// `buildSignHash()`: the message the quorum signed.
    // Core: llmq/signing.cpp:698 CSigBase::buildSignHash
    pub fn sign_hash(&self) -> [u8; 32] {
        sign_hash(self.llmq_type, &self.quorum_hash, &self.id, &self.msg_hash)
    }
}
