//! The DKG messages on the wire: the only module that names BS9a's message types and their
//! fields, and `dash_primitives` (LQ4 §6; `tests/wire_firewall.rs` checks it).
//!
//! Core's worker reads a queued payload whole under the process-wide scheme, eagerly
//! normalizing its BLS fields, then checks its structure (`l/net_dkg.cpp:370-389`). Its sign
//! hash and its session hash are `CHashWriter`s over the message as that read leaves it
//! (`l/dkgmessages.h:62-68`, `l/dkgsession.cpp:725`). The hashes here go through
//! dash-consensus's [`CoreWrite`], the path LQ2's commitment hash takes (DEC-623 Q-1), never
//! through base-sdk's encoder.

use dash_consensus::primitives::{CoreWrite, HashWriter, write_bytes, write_compact_size};
use dash_evo::types::ProTxHash;
use dash_p2p_core::{
    DkgComplaint, DkgContribution, DkgJustification, DkgMessage, DkgPrematureCommitment,
};
use dash_params::LlmqParams;
use dash_pkc::bls::{BlsIesMultiBytes, BlsPkBytes, BlsSigBytes};
use dash_primitives::TxHash;
use dash_types::Numeric;

use super::bls::{BlsCtx, BlsIesMultiBlobs, BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue};

pub use dash_primitives::{LlmqType, QuorumHash};

/// Why the worker's read of a queued payload failed (`l/net_dkg.cpp:370-389`). Either costs
/// the sender 100, per message (:405-408).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// `ds >> *msg` threw (a BLS field canonical in neither scheme included), or bytes were
    /// left over: "failed to deserialize message".
    Deserialize,
    /// `CheckDKGMessageStructure` refused it: "message failed structure check".
    Structure,
}

/// `ds >> *msg` then `ds.empty()`: base-sdk's whole read, then Core's eager read of the BLS
/// fields under `bls` (BS9a `DkgMessage::eager`).
fn read_whole<M: DkgMessage>(payload: &[u8], bls: BlsCtx) -> Result<M, ReadFailure> {
    let mut data = payload;
    let msg = M::decode(&mut data).map_err(|_| ReadFailure::Deserialize)?;
    if !data.is_empty() {
        return Err(ReadFailure::Deserialize);
    }
    msg.eager(bls).map_err(|_| ReadFailure::Deserialize)
}

/// A quorum parameter as a size: negative counts as zero (`l/net_dkg.cpp:188-190`).
fn size_param(value: i32) -> usize {
    usize::try_from(value).unwrap_or(0)
}

/// An eagerly read public key: all zeros is the invalid object, anything else is canonical in
/// the read's scheme.
fn pk_value(bytes: &[u8; 48], scheme: BlsSchemeKind) -> BlsPkValue {
    BlsPkValue::from_canonical(bytes, scheme).unwrap_or_else(BlsPkValue::invalid)
}

/// An eagerly read signature, as [`pk_value`].
fn sig_value(bytes: &[u8; 96], scheme: BlsSchemeKind) -> BlsSigValue {
    BlsSigValue::from_canonical(bytes, scheme).unwrap_or_else(BlsSigValue::invalid)
}

/// An eagerly read secret key, as [`pk_value`].
fn sk_value(bytes: &[u8; 32], scheme: BlsSchemeKind) -> BlsSkValue {
    BlsSkValue::from_canonical(bytes, scheme).unwrap_or_else(BlsSkValue::invalid)
}

fn pro_tx(hash: &TxHash) -> ProTxHash {
    ProTxHash::from(*hash.as_bytes())
}

/// `llmqType`, `quorumHash` and `proTxHash`, which start every DKG message.
fn write_prefix(
    t: LlmqType,
    quorum_hash: &QuorumHash,
    pro_tx_hash: &TxHash,
    w: &mut impl CoreWrite,
) {
    w.write(&[t.to_base()]);
    w.write(quorum_hash.as_bytes());
    w.write(pro_tx_hash.as_bytes());
}

/// `CDKGContribution::SerializeWithoutSig` (`l/dkgmessages.h:31-39`).
fn write_contribution_without_sig(c: &DkgContribution, w: &mut impl CoreWrite) {
    write_prefix(c.llmq_type, &c.quorum_hash, &c.pro_tx_hash, w);
    write_compact_size(c.vvec.len() as u64, w);
    for pk in &c.vvec {
        w.write(pk.as_bytes());
    }
    let shares = &c.contributions;
    w.write(shares.ephemeral_pk().as_bytes());
    w.write(shares.iv_seed());
    write_compact_size(shares.blobs().len() as u64, w);
    for blob in shares.blobs() {
        write_bytes(blob, w);
    }
}

/// A DKG contribution (`CDKGContribution`, `qcontrib`) as Core's worker holds it: the message as
/// its eager read leaves it, its BLS values, and its two hashes under the scheme of the read.
#[derive(Clone, Debug)]
pub struct Contribution {
    msg: DkgContribution,
    pro_tx_hash: ProTxHash,
    vvec: Vec<BlsPkValue>,
    blobs: BlsIesMultiBlobs,
    sig: BlsSigValue,
    sign_hash: [u8; 32],
    session_hash: [u8; 32],
}

impl Contribution {
    /// The worker's read (`DeserializeCheckedDKGMessage<CDKGContribution>`) of a queued
    /// payload under the process-wide scheme `bls`: the whole read, then
    /// `CheckDKGMessageStructure` (`l/net_dkg.cpp:186-192`): `vvec` holds `threshold` keys and
    /// there are `minSize..=size` blobs.
    ///
    /// # Errors
    ///
    /// [`ReadFailure`] where Core's read fails.
    pub fn read(payload: &[u8], params: &LlmqParams, bls: BlsCtx) -> Result<Self, ReadFailure> {
        let msg: DkgContribution = read_whole(payload, bls)?;
        let blobs = msg.contributions.blobs().len();
        if msg.vvec.len() != size_param(params.threshold)
            || blobs < size_param(params.min_size)
            || blobs > size_param(params.size)
        {
            return Err(ReadFailure::Structure);
        }
        Ok(Self::from_eager(msg, bls.scheme()))
    }

    /// The session's own contribution before its signature (`a/dkgsession.cpp:71-97`), its
    /// BLS fields serialized under `flag`, Core's process-wide scheme, and its signature the
    /// invalid object.
    pub(crate) fn unsigned(
        t: LlmqType,
        quorum_hash: &QuorumHash,
        pro_tx_hash: &ProTxHash,
        vvec: &[BlsPkValue],
        blobs: &BlsIesMultiBlobs,
        flag: BlsSchemeKind,
    ) -> Self {
        let msg = DkgContribution {
            llmq_type: t,
            quorum_hash: *quorum_hash,
            pro_tx_hash: TxHash::from(*pro_tx_hash.as_bytes()),
            vvec: vvec.iter().map(|pk| BlsPkBytes::from_bytes(pk.to_bytes(flag))).collect(),
            contributions: BlsIesMultiBytes::new(
                BlsPkBytes::from_bytes(blobs.ephemeral.to_bytes(flag)),
                blobs.iv_seed,
                blobs.blobs.clone(),
            ),
            sig: BlsSigBytes::from_bytes(BlsSigValue::invalid().to_bytes(flag)),
        };
        Self::from_eager(msg, flag)
    }

    /// The contribution with `sig` (`qc.sig = Sign(GetSignHash())`, `a/dkgsession.cpp:99`).
    pub(crate) fn with_sig(mut self, sig: BlsSigValue, flag: BlsSchemeKind) -> Self {
        self.msg.sig = BlsSigBytes::from_bytes(sig.to_bytes(flag));
        self.sig = sig;
        self.session_hash = Self::hash(&self.msg, true);
        self
    }

    fn from_eager(msg: DkgContribution, scheme: BlsSchemeKind) -> Self {
        let shares = &msg.contributions;
        Self {
            pro_tx_hash: pro_tx(&msg.pro_tx_hash),
            vvec: msg.vvec.iter().map(|pk| pk_value(pk.as_bytes(), scheme)).collect(),
            blobs: BlsIesMultiBlobs {
                ephemeral: pk_value(shares.ephemeral_pk().as_bytes(), scheme),
                iv_seed: *shares.iv_seed(),
                blobs: shares.blobs().to_vec(),
            },
            sig: sig_value(msg.sig.as_bytes(), scheme),
            sign_hash: Self::hash(&msg, false),
            session_hash: Self::hash(&msg, true),
            msg,
        }
    }

    /// `GetSignHash()` (`with_sig = false`: the signature field left at its default value) or
    /// `::SerializeHash(qc)`.
    fn hash(msg: &DkgContribution, with_sig: bool) -> [u8; 32] {
        let mut w = HashWriter::new();
        write_contribution_without_sig(msg, &mut w);
        if with_sig {
            w.write(msg.sig.as_bytes());
        } else {
            w.write(&[0; 96]);
        }
        w.finish()
    }

    /// The message, its BLS fields as the read left them.
    pub fn message(&self) -> &DkgContribution {
        &self.msg
    }

    /// Core's serialization of the message (`ds << qc`): the payload DK1 enqueues and serves.
    pub fn payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        write_contribution_without_sig(&self.msg, &mut out);
        out.extend_from_slice(self.msg.sig.as_bytes());
        out
    }

    /// `llmqType`.
    pub fn llmq_type(&self) -> LlmqType {
        self.msg.llmq_type
    }

    /// `quorumHash`.
    pub fn quorum_hash(&self) -> &QuorumHash {
        &self.msg.quorum_hash
    }

    /// `proTxHash`.
    pub fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }

    /// `*vvec`.
    pub fn vvec(&self) -> &[BlsPkValue] {
        &self.vvec
    }

    /// `*contributions`.
    pub fn blobs(&self) -> &BlsIesMultiBlobs {
        &self.blobs
    }

    /// `sig`.
    pub fn sig(&self) -> &BlsSigValue {
        &self.sig
    }

    /// `GetSignHash()`.
    pub fn sign_hash(&self) -> &[u8; 32] {
        &self.sign_hash
    }

    /// `::SerializeHash(qc)`: the session's key for the message and its inv hash.
    pub fn session_hash(&self) -> &[u8; 32] {
        &self.session_hash
    }
}

/// A DKG complaint (`CDKGComplaint`, `qcomplaint`) as its PreVerify reads it.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4a-2 or LQ4b-1"))]
pub struct Complaint {
    msg: DkgComplaint,
    pro_tx_hash: ProTxHash,
    sig: BlsSigValue,
}

#[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4a-2 or LQ4b-1"))]
impl Complaint {
    /// The worker's read, then `CheckDKGMessageStructure` (`l/net_dkg.cpp:194-198`): two
    /// bitsets of one size, at most `size`.
    ///
    /// # Errors
    ///
    /// [`ReadFailure`] where Core's read fails.
    pub fn read(payload: &[u8], params: &LlmqParams, bls: BlsCtx) -> Result<Self, ReadFailure> {
        let msg: DkgComplaint = read_whole(payload, bls)?;
        let bits = msg.bad_members.num_bits;
        if bits != msg.complain_for_members.num_bits || bits > size_param(params.size) as u64 {
            return Err(ReadFailure::Structure);
        }
        Ok(Self {
            pro_tx_hash: pro_tx(&msg.pro_tx_hash),
            sig: sig_value(msg.sig.as_bytes(), bls.scheme()),
            msg,
        })
    }

    pub(crate) fn quorum_hash(&self) -> &QuorumHash {
        &self.msg.quorum_hash
    }

    pub(crate) fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }

    pub(crate) fn sig(&self) -> &BlsSigValue {
        &self.sig
    }

    /// `badMembers.size()`.
    pub(crate) fn bad_members_len(&self) -> u64 {
        self.msg.bad_members.num_bits
    }

    /// `complainForMembers.size()`.
    pub(crate) fn complain_for_len(&self) -> u64 {
        self.msg.complain_for_members.num_bits
    }
}

/// A DKG justification (`CDKGJustification`, `qjustify`) as its PreVerify reads it.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4a-2 or LQ4b-1"))]
pub struct Justification {
    msg: DkgJustification,
    pro_tx_hash: ProTxHash,
    sig: BlsSigValue,
    keys: Vec<(u32, BlsSkValue)>,
}

#[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4a-2 or LQ4b-1"))]
impl Justification {
    /// The worker's read, then `CheckDKGMessageStructure` (`l/net_dkg.cpp:200-204`): at most
    /// `size` revealed shares.
    ///
    /// # Errors
    ///
    /// [`ReadFailure`] where Core's read fails.
    pub fn read(payload: &[u8], params: &LlmqParams, bls: BlsCtx) -> Result<Self, ReadFailure> {
        let msg: DkgJustification = read_whole(payload, bls)?;
        if msg.contributions.len() > size_param(params.size) {
            return Err(ReadFailure::Structure);
        }
        Ok(Self {
            pro_tx_hash: pro_tx(&msg.pro_tx_hash),
            sig: sig_value(msg.sig.as_bytes(), bls.scheme()),
            keys: msg
                .contributions
                .iter()
                .map(|share| (share.index, sk_value(&share.key, bls.scheme())))
                .collect(),
            msg,
        })
    }

    pub(crate) fn quorum_hash(&self) -> &QuorumHash {
        &self.msg.quorum_hash
    }

    pub(crate) fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }

    pub(crate) fn sig(&self) -> &BlsSigValue {
        &self.sig
    }

    /// `contributions`: each revealed share's index and key.
    pub(crate) fn keys(&self) -> &[(u32, BlsSkValue)] {
        &self.keys
    }
}

/// A premature commitment (`CDKGPrematureCommitment`, `qpcommit`) as its PreVerify reads it.
#[derive(Clone, Debug)]
#[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4a-2 or LQ4b-1"))]
pub struct PrematureCommitment {
    msg: DkgPrematureCommitment,
    pro_tx_hash: ProTxHash,
    sig: BlsSigValue,
    quorum_sig: BlsSigValue,
}

#[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4a-2 or LQ4b-1"))]
impl PrematureCommitment {
    /// The worker's read, then `CheckDKGMessageStructure` (`l/net_dkg.cpp:206-210`): at most
    /// `size` bits.
    ///
    /// # Errors
    ///
    /// [`ReadFailure`] where Core's read fails.
    pub fn read(payload: &[u8], params: &LlmqParams, bls: BlsCtx) -> Result<Self, ReadFailure> {
        let msg: DkgPrematureCommitment = read_whole(payload, bls)?;
        if msg.valid_members.num_bits > size_param(params.size) as u64 {
            return Err(ReadFailure::Structure);
        }
        Ok(Self {
            pro_tx_hash: pro_tx(&msg.pro_tx_hash),
            sig: sig_value(msg.sig.as_bytes(), bls.scheme()),
            quorum_sig: sig_value(msg.quorum_sig.as_bytes(), bls.scheme()),
            msg,
        })
    }

    pub(crate) fn quorum_hash(&self) -> &QuorumHash {
        &self.msg.quorum_hash
    }

    pub(crate) fn pro_tx_hash(&self) -> &ProTxHash {
        &self.pro_tx_hash
    }

    /// `sig`, the member's signature ("membersSig" in Core's log text).
    pub(crate) fn sig(&self) -> &BlsSigValue {
        &self.sig
    }

    pub(crate) fn quorum_sig(&self) -> &BlsSigValue {
        &self.quorum_sig
    }

    /// `validMembers.size()`.
    pub(crate) fn valid_members_len(&self) -> u64 {
        self.msg.valid_members.num_bits
    }

    /// `CountValidMembers()`.
    pub(crate) fn count_valid_members(&self) -> u64 {
        self.msg.valid_members.count_ones()
    }

    /// `validMembers[i]`; `false` past the end.
    pub(crate) fn valid_member(&self, i: u64) -> bool {
        self.msg.valid_members.get(i).unwrap_or(false)
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
