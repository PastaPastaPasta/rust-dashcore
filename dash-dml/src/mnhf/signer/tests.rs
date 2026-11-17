use std::cell::Cell;

use dash_consensus::primitives::CoreSerialize;
use dash_params::{ChainParamsBuilder, Network, Release};

use super::*;
use crate::testkit::operator_sig;

const BIT: u8 = 13;
const START: i64 = 1_700_000_000;
const END: i64 = START + 10;

fn params() -> ChainSetup {
    let mut p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    for pos in [DeploymentPos::TestDummy, DeploymentPos::V24, DeploymentPos::EvoShares] {
        let deployment = p.consensus.deployments.deployment_mut(pos).unwrap();
        deployment.use_ehf = pos != DeploymentPos::TestDummy;
        deployment.start_time = START;
        deployment.timeout = END;
    }
    p
}

fn request_id(bit: u8) -> [u8; 32] {
    *unsigned_payload(bit, QuorumHash::from([0; 32])).request_id().as_bytes()
}

fn expect_sign(outcome: TrySignOutcome) -> SignRequest {
    match outcome {
        TrySignOutcome::Sign(request) => request,
        other => panic!("expected signing request, got {other:?}"),
    }
}

fn tracked(p: &ChainSetup, bit: u8) -> EhfSigner {
    let mut signer = EhfSigner::default();
    assert_eq!(
        ehf_try_sign(p, bit, &mut signer, |_| panic!("already recovered"), |_, _| true),
        TrySignOutcome::AlreadyRecovered
    );
    signer
}

fn eligible_bits(
    p: &ChainSetup,
    active: bool,
    mtp: i64,
    mined: &EhfSignals,
) -> Vec<(u8, [u8; 32])> {
    ehf_sign_requests(
        p,
        TipFacts {
            v20_active: active,
            mtp,
            mined_bits: mined,
        },
    )
}

#[test]
fn v20_gate_and_evo_shares_skip() {
    let p = params();
    let mined = EhfSignals::new();
    assert!(eligible_bits(&p, false, START, &mined).is_empty());
    assert_eq!(eligible_bits(&p, true, START, &mined), vec![(BIT, request_id(BIT))]);
    assert!(
        p.consensus.deployments.iter().any(|(pos, d)| pos == DeploymentPos::EvoShares && d.use_ehf)
    );
}

#[test]
fn signal_window_edges_and_never_active() {
    let mut p = params();
    let mined = EhfSignals::new();
    for (mtp, count) in [(START - 1, 0), (START, 1), (END, 1), (END + 1, 0)] {
        assert_eq!(eligible_bits(&p, true, mtp, &mined).len(), count, "mtp={mtp}");
    }
    p.consensus.deployments.deployment_mut(DeploymentPos::V24).unwrap().start_time =
        Bip9Deployment::NEVER_ACTIVE;
    assert!(is_valid_mn_activation(&p, BIT, START));
    assert!(eligible_bits(&p, true, START, &mined).is_empty());
}

#[test]
fn mined_bits_and_non_mn_activation_are_skipped() {
    let mut p = params();
    let mined = [(BIT, 20)].into_iter().collect();
    assert!(eligible_bits(&p, true, START, &mined).is_empty());
    p.consensus.deployments.deployment_mut(DeploymentPos::TestDummy).unwrap().bit = i32::from(BIT);
    assert!(!is_valid_mn_activation(&p, BIT, START));
    assert!(eligible_bits(&p, true, START, &EhfSignals::new()).is_empty());
}

#[test]
fn sign_request_tracks_only_recovered_or_selected_ids() {
    let mut p = params();
    let mut signer = EhfSigner::default();
    let id = request_id(BIT);
    let selected = Cell::new(false);
    let lookup = Cell::new(false);
    let select = |seen: &[u8; 32]| {
        assert_eq!(seen, &id);
        selected.set(true);
        Some(QuorumHash::from([7; 32]))
    };
    let has = |t, seen: &[u8; 32]| {
        assert_eq!((t, seen), (p.consensus.llmq_type_mnhf, &id));
        lookup.set(true);
        true
    };
    assert_eq!(ehf_try_sign(&p, BIT, &mut signer, select, has), TrySignOutcome::AlreadyRecovered);
    assert!(lookup.get() && !selected.get());
    assert!(signer.tracks(&id));
    signer = EhfSigner::default();
    assert_eq!(
        ehf_try_sign(&p, BIT, &mut signer, |_| None, |_, _| false),
        TrySignOutcome::NoQuorum
    );
    assert!(!signer.tracks(&id));
    let req = expect_sign(ehf_try_sign(&p, BIT, &mut signer, select, |_, _| false));
    assert_eq!(
        req,
        SignRequest {
            llmq_type: p.consensus.llmq_type_mnhf,
            id,
            msg_hash: *Tx::new(prepare_tx(&unsigned_payload(BIT, QuorumHash::from([7; 32]))))
                .txid()
                .as_bytes(),
            quorum_hash: QuorumHash::from([7; 32]),
            allow_diff_msg: true,
        }
    );
    assert!(signer.tracks(&id));
    signer = EhfSigner::default();
    p.consensus.llmq_type_mnhf = LlmqType::new(0xff);
    assert_eq!(
        ehf_try_sign(&p, BIT, &mut signer, |_| panic!("selection"), |_, _| panic!("lookup")),
        TrySignOutcome::UnconfiguredQuorumType
    );
    assert!(!signer.tracks(&id));
}

fn recovered(p: &ChainSetup, bit: u8) -> RecoveredSig {
    RecoveredSig {
        llmq_type: p.consensus.llmq_type_mnhf,
        quorum_hash: QuorumHash::from([7; 32]),
        id: request_id(bit),
        msg_hash: [8; 32],
        sig: operator_sig(1, &[9; 32]).to_bytes(BlsSchemeKind::Basic),
    }
}

#[test]
fn recovered_filters_tracked_ids_deployments_and_mined_bits() {
    let p = params();
    let rec = recovered(&p, BIT);
    let signer = tracked(&p, BIT);
    let empty = EhfSignals::new();
    assert!(ehf_on_recovered(&p, &rec, &EhfSigner::default(), &empty, BlsCtx::BASIC).is_none());
    let mut wrong = rec.clone();
    wrong.id = request_id(5);
    let wrong_signer = tracked(&p, 5);
    assert!(ehf_on_recovered(&p, &wrong, &signer, &empty, BlsCtx::BASIC).is_none());
    assert!(ehf_on_recovered(&p, &wrong, &wrong_signer, &empty, BlsCtx::BASIC).is_none());
    let non_ehf = recovered(&p, 28);
    let non_ehf_signer = tracked(&p, 28);
    assert!(ehf_on_recovered(&p, &non_ehf, &non_ehf_signer, &empty, BlsCtx::BASIC).is_none());
    let mined = [(BIT, 20)].into_iter().collect();
    assert!(ehf_on_recovered(&p, &rec, &signer, &mined, BlsCtx::BASIC).is_none());
    let tx = ehf_on_recovered(&p, &rec, &signer, &empty, BlsCtx::BASIC).unwrap();
    let tx = Tx::new(tx);
    let decoded = tx.payload().unwrap().unwrap();
    let dash_primitives::SpecialPayload::MnhfSignal(payload) = decoded.payload() else {
        panic!("MNHF payload")
    };
    assert_eq!(payload.version_bit(), BIT);
    assert_eq!(payload.quorum_hash(), rec.quorum_hash);
    assert_eq!(payload.sig().to_bytes(BlsSchemeKind::Basic), rec.sig);
}

#[test]
fn recovered_does_not_reapply_signing_gates() {
    let mut p = params();
    let deployment = p.consensus.deployments.deployment_mut(DeploymentPos::EvoShares).unwrap();
    deployment.start_time = Bip9Deployment::NEVER_ACTIVE;
    let bit = deployment.bit as u8;
    let mut rec = recovered(&p, bit);
    rec.llmq_type = LlmqType::new(0xff);
    let signer = tracked(&p, bit);
    assert!(ehf_on_recovered(&p, &rec, &signer, &EhfSignals::new(), BlsCtx::BASIC).is_some());
}

#[test]
fn recovered_signature_is_written_in_basic_scheme() {
    let p = params();
    let mut rec = recovered(&p, BIT);
    let basic = rec.sig;
    rec.sig = operator_sig(1, &[9; 32]).to_bytes(BlsSchemeKind::Legacy);
    assert_ne!(rec.sig, basic);
    let signer = tracked(&p, BIT);
    let tx = ehf_on_recovered(&p, &rec, &signer, &EhfSignals::new(), BlsCtx::LEGACY).unwrap();
    assert_eq!(&tx.extra_payload()[34..], &basic);
}

#[test]
fn prepare_tx_matches_hand_serialized_core_bytes() {
    // Core v24.0.0 evo/mnhftx.cpp:37-45 sets version 3/type 7 on the default transaction.
    // primitives/transaction.h:361-372 writes packed version/type (03000700), CompactSize
    // vin/vout (00/00), uint32 locktime (00000000), CompactSize payload size (82 = 130).
    // evo/mnhftx.h:46-50,84-87 writes uint8 version 1, bit 13, the uint256's 32 internal
    // bytes (00..1f), and the default CBLSSignature as 96 zeros. No Rust encoder builds this
    // expected value; the segments below are that serialization written by hand.
    let expected_hex = concat!(
        "03000700",
        "00",
        "00",
        "00000000",
        "82",
        "01",
        "0d",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    let expected: Vec<u8> = expected_hex
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect();
    let q = QuorumHash::from(std::array::from_fn(|i| i as u8));
    let payload = unsigned_payload(BIT, q);
    let tx = prepare_tx(&payload);
    assert_eq!(expected.len(), 141);
    assert_eq!(tx.core_serialize(), expected);
    let mut signer = EhfSigner::default();
    let req = expect_sign(ehf_try_sign(&params(), BIT, &mut signer, |_| Some(q), |_, _| false));
    assert_eq!(req.msg_hash, dash_consensus::hash::sha256d(&expected));
    // Core GetRequestId serializes pair<string,int64_t>: 04 "mnhf" then int64 bit 13.
    assert_eq!(req.id, dash_consensus::hash::sha256d(b"\x04mnhf\x0d\0\0\0\0\0\0\0"));
}

#[test]
fn non_signing_node_retains_id_for_later_recovered_callback() {
    let p = params();
    let rec = recovered(&p, BIT);
    let mut signer = EhfSigner::default();
    let request =
        expect_sign(ehf_try_sign(&p, BIT, &mut signer, |_| Some(rec.quorum_hash), |_, _| false));
    let sign_if_member = |_: &SignRequest| false;
    assert!(!sign_if_member(&request));
    assert!(signer.tracks(&rec.id));
    let mined = EhfSignals::new();
    assert!(ehf_on_recovered(&p, &rec, &signer, &mined, BlsCtx::BASIC).is_some());
    assert!(ehf_on_recovered(&p, &rec, &EhfSigner::default(), &mined, BlsCtx::BASIC).is_none());
}
