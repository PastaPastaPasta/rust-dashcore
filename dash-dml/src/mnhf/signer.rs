//! Pure decisions from Core's `CEHFSignalsHandler`; the caller supplies `GetSignalsStage(tip)`
//! and passes returned unsent transactions to the runtime for mempool acceptance and relay.

use std::collections::BTreeSet;

use dash_consensus::Tx;
use dash_evo::bls::BlsCtx;
use dash_evo::locks::wire::RecoveredSig;
use dash_params::{Bip9Deployment, ChainSetup, DeploymentPos, LlmqType};
use dash_pkc::bls::{BlsEagerSig, BlsLazySig, BlsSchemeKind, BlsSchemeSource};
use dash_primitives::{MnHardFork, QuorumHash, Transaction, TxType};

use super::{EhfSignals, is_valid_mn_activation};

/// `DeploymentActiveAfter(tip, V20)`, the tip's median time past, and `GetSignalsStage(tip)`.
#[derive(Clone, Copy, Debug)]
pub struct TipFacts<'a> {
    pub v20_active: bool,
    pub mtp: i64,
    pub mined_bits: &'a EhfSignals,
}

/// The EHF call to `AsyncSignIfMember`: `allowReSign` is false and
/// `allowDiffMsgHashSigning` is true. The signing manager still checks membership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignRequest {
    pub llmq_type: LlmqType,
    pub id: [u8; 32],
    pub msg_hash: [u8; 32],
    pub quorum_hash: QuorumHash,
    pub allow_diff_msg: bool,
}

/// Handler state, retained across tip updates and recovered-signature callbacks. An ID is
/// tracked before the signing manager decides whether this node is a signing member.
// Core: v24.0.0 llmq/ehf_signals.h:29-33 CEHFSignalsHandler::ids
#[derive(Clone, Debug, Default)]
pub struct EhfSigner {
    ids: BTreeSet<[u8; 32]>,
}

impl EhfSigner {
    // Core: v24.0.0 llmq/ehf_signals.cpp:106-109 CEHFSignalsHandler::HandleNewRecoveredSig
    pub fn tracks(&self, id: &[u8; 32]) -> bool {
        self.ids.contains(id)
    }
}

/// Why an eligible bit did or did not produce a signing request. Both `AlreadyRecovered` and
/// `Sign` track the ID in [`EhfSigner`], independent of the eventual signing decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrySignOutcome {
    UnconfiguredQuorumType,
    AlreadyRecovered,
    NoQuorum,
    Sign(SignRequest),
}

// Core: v24.0.0 llmq/ehf_signals.cpp:36-40 IsSignalWindowOpen
fn is_signal_window_open(deployment: &Bip9Deployment, mtp: i64) -> bool {
    deployment.start_time != Bip9Deployment::NEVER_ACTIVE
        && deployment.start_time <= mtp
        && mtp <= deployment.timeout
}

// Core: v24.0.0 evo/mnhftx.h:40-42,64-65 MNHFTxPayload default initialization
fn unsigned_payload(bit: u8, quorum_hash: QuorumHash) -> MnHardFork {
    let sig =
        BlsLazySig::from_bytes(&[0; 96], BlsSchemeSource::Version(BlsSchemeKind::Basic)).get();
    MnHardFork::new(1, bit, quorum_hash, BlsEagerSig::new(sig, BlsSchemeKind::Basic))
}

/// `MNHFTxPayload::PrepareTx`: version 3, type 7, empty inputs and outputs, lock time zero.
/// The signature is always written in the basic scheme (`MNHFTx::SERIALIZE_METHODS`).
// Core: v24.0.0 evo/mnhftx.cpp:37-45 MNHFTxPayload::PrepareTx
// Core: v24.0.0 evo/mnhftx.h:46-50,84-87 MNHFTxPayload SERIALIZE_METHODS
pub fn prepare_tx(payload: &MnHardFork) -> Transaction {
    let mut bytes = Vec::with_capacity(130);
    bytes.push(payload.version());
    bytes.push(payload.version_bit());
    bytes.extend_from_slice(payload.quorum_hash().as_bytes());
    bytes.extend_from_slice(&payload.sig().to_bytes(BlsSchemeKind::Basic));
    Transaction::new(3, TxType::MnhfSignal, vec![], vec![], 0, bytes)
}

/// Bits eligible for signing, in deployment order, with the existing payload's request ID.
/// `mined_bits` must be the signals stage, including its expiration rules, rather than every
/// signal ever mined. This function does not add IDs to the handler's tracked set.
// Core: v24.0.0 llmq/ehf_signals.cpp:42-64 CEHFSignalsHandler::UpdatedBlockTip
pub fn ehf_sign_requests(params: &ChainSetup, tip: TipFacts<'_>) -> Vec<(u8, [u8; 32])> {
    if !tip.v20_active {
        return Vec::new();
    }
    params
        .consensus
        .deployments
        .iter()
        .filter(|(pos, deployment)| {
            deployment.use_ehf
                && *pos != DeploymentPos::EvoShares
                && is_signal_window_open(deployment, tip.mtp)
        })
        .filter_map(|(_, deployment)| {
            let bit = deployment.bit as u8;
            (tip.mined_bits.get(bit).is_none() && is_valid_mn_activation(params, bit, tip.mtp))
                .then(|| {
                    let payload = unsigned_payload(bit, QuorumHash::from([0; 32]));
                    (bit, *payload.request_id().as_bytes())
                })
        })
        .collect()
}

/// Try one request from [`ehf_sign_requests`]. The quorum selector sees the request ID;
/// the recovered-signature lookup sees the MNHF quorum type and that ID. The selector must
/// use Core's default signing offset of 8 blocks from the active tip (`SelectQuorumForSigning`,
/// `llmq/quorumsman.h:214-223`), rather than selecting directly at the tip.
///
/// Core tracks the ID both when a recovered signature already exists and when a quorum was
/// selected. It does not track it when the quorum type is unconfigured or selection fails.
// Core: v24.0.0 llmq/ehf_signals.cpp:66-98 CEHFSignalsHandler::trySignEHFSignal
pub fn ehf_try_sign(
    params: &ChainSetup,
    bit: u8,
    signer: &mut EhfSigner,
    select: impl FnOnce(&[u8; 32]) -> Option<QuorumHash>,
    has_recsig_for_id: impl FnOnce(LlmqType, &[u8; 32]) -> bool,
) -> TrySignOutcome {
    let llmq_type = params.consensus.llmq_type_mnhf;
    if params.consensus.llmq(llmq_type).is_none() {
        return TrySignOutcome::UnconfiguredQuorumType;
    }
    let payload = unsigned_payload(bit, QuorumHash::from([0; 32]));
    let id = *payload.request_id().as_bytes();
    if has_recsig_for_id(llmq_type, &id) {
        signer.ids.insert(id);
        return TrySignOutcome::AlreadyRecovered;
    }
    let Some(quorum_hash) = select(&id) else {
        return TrySignOutcome::NoQuorum;
    };
    let tx = Tx::new(prepare_tx(&unsigned_payload(bit, quorum_hash)));
    signer.ids.insert(id);
    TrySignOutcome::Sign(SignRequest {
        llmq_type,
        id,
        msg_hash: *tx.txid().as_bytes(),
        quorum_hash,
        allow_diff_msg: true,
    })
}

/// Build an unsent mempool transaction for an already verified recovered signature.
/// `read_bls` is the scheme retained by its lazy signature when received; the MNHF payload
/// writes it in basic form.
///
/// Core filters by tracked ID and unmined EHF deployment only: it does not reapply the V20,
/// `EVO_SHARES`, or signal-window signing gates here, nor check the type or message hash.
/// The caller performs mempool acceptance before relaying the returned transaction.
// Core: v24.0.0 llmq/ehf_signals.cpp:100-144 CEHFSignalsHandler::HandleNewRecoveredSig
pub fn ehf_on_recovered(
    params: &ChainSetup,
    recovered: &RecoveredSig,
    signer: &EhfSigner,
    mined_bits: &EhfSignals,
    read_bls: BlsCtx,
) -> Option<Transaction> {
    if !signer.tracks(&recovered.id) {
        return None;
    }
    for (_, deployment) in params.consensus.deployments.iter() {
        let bit = deployment.bit as u8;
        if !deployment.use_ehf || mined_bits.get(bit).is_some() {
            continue;
        }
        let payload = unsigned_payload(bit, recovered.quorum_hash);
        if payload.request_id().as_bytes() != &recovered.id {
            continue;
        }
        let sig = BlsLazySig::from_bytes(&recovered.sig, BlsSchemeSource::Global(read_bls)).get();
        let payload = MnHardFork::new(
            payload.version(),
            bit,
            recovered.quorum_hash,
            BlsEagerSig::new(sig, BlsSchemeKind::Basic),
        );
        return Some(prepare_tx(&payload));
    }
    None
}

#[cfg(test)]
mod tests;
