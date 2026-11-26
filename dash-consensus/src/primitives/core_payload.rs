//! The node-side payload side-car: a decoded special payload together with the BLS objects the
//! node reads from it (T6 §3).
//!
//! Every node site reads a provider payload's operator key and operator signature through
//! [`CorePayload::operator_key`] and [`CorePayload::operator_sig`], from the one object cached
//! with the decode ([`Tx::payload`](super::Tx::payload)), so a key resolved by an earlier check is
//! the key later checks, comparisons and clones see. base-sdk holds both fields as bytes; the
//! side-car builds each object once, at decode, and refuses the payloads Core's payload read
//! refuses.

use core::ops::Deref;

use dash_p2p_core::{MnListDiff, QrInfo};
use dash_pkc::bls::{
    BlsEager, BlsEagerPk, BlsEagerSig, BlsGlobalSig, BlsLazyPk, BlsSchemeKind, BlsSchemeSource,
};
use dash_primitives::{
    Commitment, OperatorKey, OperatorSig, PayloadDecode, PayloadError, ProTxVersion,
    SpecialPayload, TxType,
};

/// A decoded special payload and its BLS side-car.
#[derive(Clone, Debug)]
pub struct CorePayload {
    decode: PayloadDecode,
    bls: PayloadBls,
}

/// The BLS objects of one decoded payload.
#[derive(Clone, Debug)]
struct PayloadBls {
    operator_key: Option<BlsLazyPk>,
    operator_sig: Option<BlsEagerSig>,
}

impl CorePayload {
    /// Decodes the extra payload of a `tx_type` transaction from the front of `extra`, as
    /// base-sdk's `Transaction::decode_payload` does, and builds its side-car; unread bytes are
    /// not an error here (see [`Self::is_complete`]).
    ///
    /// # Errors
    ///
    /// The `PayloadError` of a recognised type that fails to decode, and Core's error for a
    /// payload whose operator signature Core's payload read refuses.
    pub fn decode(tx_type: TxType, extra: &[u8]) -> Result<CorePayload, PayloadError> {
        let mut cursor = extra;
        let payload = SpecialPayload::decode(tx_type, &mut cursor)?;
        let consumed = extra.len() - cursor.len();
        let bls = PayloadBls {
            operator_key: provider_operator_key(&payload),
            operator_sig: provider_operator_sig(tx_type, &payload)?,
        };
        Ok(Self {
            decode: PayloadDecode::new(payload, consumed, extra.len()),
            bls,
        })
    }

    /// `GetTxPayload`'s "stream empty" condition: the decoder read every byte.
    pub fn is_complete(&self) -> bool {
        self.decode.is_complete()
    }

    /// The decoded special payload.
    pub fn payload(&self) -> &SpecialPayload {
        self.decode.payload()
    }

    /// The operator key of a ProRegTx, ProUpRegTx or ProUpSharedRegTx.
    pub fn operator_key(&self) -> Option<&BlsLazyPk> {
        self.bls.operator_key.as_ref()
    }

    /// The operator signature of a ProUpServTx or ProUpRevTx.
    pub fn operator_sig(&self) -> Option<&BlsEagerSig> {
        self.bls.operator_sig.as_ref()
    }

    /// The best ChainLock signature of a CbTx, as its wire bytes: the caller resolves it under
    /// its `BlsCtx` at the point of use (T6 §11.1).
    pub fn best_cl_signature(&self) -> Option<&BlsGlobalSig> {
        match self.payload() {
            SpecialPayload::CoinbaseCommitment(cbtx) => cbtx.best_cl_signature().as_ref(),
            _ => None,
        }
    }

    /// The quorum signature of an asset unlock, as its wire bytes: the caller resolves it under
    /// its `BlsCtx` at the point of use (T6 §11.1).
    pub fn asset_unlock_quorum_sig(&self) -> Option<&BlsGlobalSig> {
        match self.payload() {
            SpecialPayload::AssetUnlock(unlock) => Some(unlock.quorum_sig()),
            _ => None,
        }
    }

    /// The signature of an MnHF signal.
    pub fn mnhf_sig(&self) -> Option<&BlsEagerSig> {
        match self.payload() {
            SpecialPayload::MnhfSignal(mnhf) => Some(mnhf.sig()),
            _ => None,
        }
    }

    /// The BLS objects of a final commitment transaction's commitment.
    pub fn commitment_bls(&self) -> Option<CommitmentBls<'_>> {
        match self.payload() {
            SpecialPayload::QuorumCommitment(qc) => Some(CommitmentBls::of(qc.commitment())),
            _ => None,
        }
    }
}

/// The BLS objects of an LLMQ final commitment, as commitment verification reads them.
#[derive(Clone, Copy, Debug)]
pub struct CommitmentBls<'a> {
    /// `quorumPublicKey`.
    pub quorum_public_key: &'a BlsEagerPk,
    /// `quorumSig`.
    pub quorum_sig: &'a BlsEagerSig,
    /// `membersSig`.
    pub members_sig: &'a BlsEagerSig,
}

impl<'a> CommitmentBls<'a> {
    /// The commitment's own fields: the delegate form, while base-sdk's `Commitment` holds the BLS
    /// objects. It goes when Q-4 turns those fields into bytes.
    pub fn of(qc: &'a Commitment) -> Self {
        Self {
            quorum_public_key: qc.quorum_public_key(),
            quorum_sig: qc.quorum_sig(),
            members_sig: qc.members_sig(),
        }
    }
}

impl Deref for CorePayload {
    type Target = PayloadDecode;

    fn deref(&self) -> &Self::Target {
        &self.decode
    }
}

/// A final commitment read from a `qfcommit` message, with its BLS side-car.
///
/// [`P2pCommitment::new`], [`P2pMnListDiff::new`] and [`P2pQrInfo::new`] are the one place the
/// BLS objects of a commitment read from a P2P message are built (T6 Q-3).
#[derive(Clone, Debug)]
pub struct P2pCommitment {
    qc: Commitment,
}

impl P2pCommitment {
    /// The side-car of a decoded commitment. In the delegate form base-sdk's decode built its BLS
    /// objects, so nothing more is held.
    pub fn new(qc: Commitment) -> Self {
        Self {
            qc,
        }
    }

    /// The commitment's BLS objects.
    pub fn bls(&self) -> CommitmentBls<'_> {
        CommitmentBls::of(&self.qc)
    }

    /// The decoded commitment.
    pub fn into_commitment(self) -> Commitment {
        self.qc
    }
}

impl Deref for P2pCommitment {
    type Target = Commitment;

    fn deref(&self) -> &Self::Target {
        &self.qc
    }
}

/// A decoded `mnlistdiff` with the side-car of its new quorums.
#[derive(Clone, Debug)]
pub struct P2pMnListDiff {
    diff: MnListDiff,
}

impl P2pMnListDiff {
    /// The side-car of a decoded diff (delegate form, as [`P2pCommitment::new`]).
    pub fn new(diff: MnListDiff) -> Self {
        Self {
            diff,
        }
    }

    /// The diff with its side-car.
    pub fn bls(&self) -> MnListDiffBls<'_> {
        MnListDiffBls::of(&self.diff)
    }

    /// The decoded diff.
    pub fn into_diff(self) -> MnListDiff {
        self.diff
    }
}

impl Deref for P2pMnListDiff {
    type Target = MnListDiff;

    fn deref(&self) -> &Self::Target {
        &self.diff
    }
}

/// A decoded `qrinfo` with the side-car of every commitment it carries.
#[derive(Clone, Debug)]
pub struct P2pQrInfo {
    info: QrInfo,
}

impl P2pQrInfo {
    /// The side-car of a decoded rotation info (delegate form, as [`P2pCommitment::new`]).
    pub fn new(info: QrInfo) -> Self {
        Self {
            info,
        }
    }

    /// `diff_tip` with its side-car.
    pub fn diff_tip_bls(&self) -> MnListDiffBls<'_> {
        MnListDiffBls::of(self.info.diff_tip())
    }

    /// `diff_h` with its side-car.
    pub fn diff_h_bls(&self) -> MnListDiffBls<'_> {
        MnListDiffBls::of(self.info.diff_h())
    }

    /// `diff_h_c` with its side-car.
    pub fn diff_h_c_bls(&self) -> MnListDiffBls<'_> {
        MnListDiffBls::of(self.info.diff_h_c())
    }

    /// `diff_h_2c` with its side-car.
    pub fn diff_h_2c_bls(&self) -> MnListDiffBls<'_> {
        MnListDiffBls::of(self.info.diff_h_2c())
    }

    /// `diff_h_3c` with its side-car.
    pub fn diff_h_3c_bls(&self) -> MnListDiffBls<'_> {
        MnListDiffBls::of(self.info.diff_h_3c())
    }

    /// The H-4C cycle's diff with its side-car, when the extra share is present.
    pub fn diff_h_4c_bls(&self) -> Option<MnListDiffBls<'_>> {
        self.info.cycle_h_4c().as_ref().map(|cycle| MnListDiffBls::of(cycle.diff()))
    }

    /// `mn_list_diff_list` with the side-car of each diff, in wire order.
    pub fn mn_list_diff_list_bls(&self) -> impl ExactSizeIterator<Item = MnListDiffBls<'_>> {
        self.info.mn_list_diff_list().iter().map(MnListDiffBls::of)
    }

    /// `last_commitment_per_index`, each with its BLS objects, in wire order.
    pub fn last_commitment_bls(
        &self,
    ) -> impl ExactSizeIterator<Item = (&Commitment, CommitmentBls<'_>)> {
        self.info.last_commitment_per_index().iter().map(|qc| (qc, CommitmentBls::of(qc)))
    }

    /// The decoded rotation info.
    pub fn into_info(self) -> QrInfo {
        self.info
    }
}

impl Deref for P2pQrInfo {
    type Target = QrInfo;

    fn deref(&self) -> &Self::Target {
        &self.info
    }
}

/// One diff of a P2P message, with the side-car of its new quorums.
#[derive(Clone, Copy, Debug)]
pub struct MnListDiffBls<'a> {
    diff: &'a MnListDiff,
}

impl<'a> MnListDiffBls<'a> {
    fn of(diff: &'a MnListDiff) -> Self {
        Self {
            diff,
        }
    }

    /// `new_quorums`, each with its BLS objects, in wire order.
    pub fn new_quorum_bls(
        &self,
    ) -> impl ExactSizeIterator<Item = (&'a Commitment, CommitmentBls<'a>)> + use<'a> {
        self.diff.new_quorums().iter().map(|qc| (qc, CommitmentBls::of(qc)))
    }

    /// `new_quorums`, each as an owned [`P2pCommitment`] holding this read's side-car, in wire
    /// order: a holder keeps the side-car the read built and builds none of its own.
    pub fn new_quorum_reads(&self) -> impl ExactSizeIterator<Item = P2pCommitment> + use<'a> {
        self.diff.new_quorums().iter().map(|qc| P2pCommitment::new(qc.clone()))
    }
}

impl Deref for MnListDiffBls<'_> {
    type Target = MnListDiff;

    fn deref(&self) -> &Self::Target {
        self.diff
    }
}

/// The scheme a provider payload's BLS fields (types 1 to 4) decode and encode under: legacy for
/// version 1 only (`nVersion == ProTxVersion::LegacyBLS`).
#[must_use]
pub const fn protx_key_scheme(version: ProTxVersion) -> BlsSchemeKind {
    BlsSchemeKind::from_legacy(matches!(version, ProTxVersion::LegacyBls))
}

/// The operator key object of a ProRegTx, ProUpRegTx or ProUpSharedRegTx.
fn provider_operator_key(payload: &SpecialPayload) -> Option<BlsLazyPk> {
    Some(match payload {
        SpecialPayload::ProviderRegister(p) => {
            pro_tx_operator_key(&p.pub_key_operator(), p.version())
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            pro_tx_operator_key(&p.pub_key_operator(), p.version())
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            shared_operator_key(&p.pub_key_operator())
        }
        _ => return None,
    })
}

/// The operator key of a ProRegTx or ProUpRegTx of `version`: its bytes under the version's
/// scheme (Core's `CBLSLazyWrapper::Unserialize`).
#[must_use]
pub fn pro_tx_operator_key(key: &OperatorKey, version: ProTxVersion) -> BlsLazyPk {
    BlsLazyPk::from_bytes(key.as_bytes().as_bytes(), version_source(version))
}

/// The operator key of a ProUpSharedRegTx: its bytes under the basic scheme always.
#[must_use]
pub fn shared_operator_key(key: &OperatorKey) -> BlsLazyPk {
    BlsLazyPk::from_bytes(key.as_bytes().as_bytes(), BlsSchemeSource::AlwaysBasic)
}

/// The operator signature object of a ProUpServTx or ProUpRevTx under the version's scheme (Core's
/// `CBLSWrapper::Unserialize`).
fn provider_operator_sig(
    tx_type: TxType,
    payload: &SpecialPayload,
) -> Result<Option<BlsEagerSig>, PayloadError> {
    let (sig, version) = match payload {
        SpecialPayload::ProviderUpdateService(p) => (p.sig(), p.version()),
        SpecialPayload::ProviderUpdateRevoke(p) => (p.sig(), p.version()),
        _ => return Ok(None),
    };
    pro_tx_operator_sig(&sig, version)
        .map(Some)
        .ok_or_else(|| PayloadError::new(tx_type, "malleable BLS object".into()))
}

/// The operator signature of a ProUpServTx or ProUpRevTx of `version`: its bytes under the
/// version's scheme, or under the other scheme when canonical only there (Core's
/// `CBLSWrapper::Unserialize`); `None` when Core's payload read refuses it.
#[must_use]
pub fn pro_tx_operator_sig(sig: &OperatorSig, version: ProTxVersion) -> Option<BlsEagerSig> {
    BlsEager::decode(sig.as_bytes().as_bytes(), version_source(version)).ok()
}

const fn version_source(version: ProTxVersion) -> BlsSchemeSource {
    BlsSchemeSource::Version(protx_key_scheme(version))
}
