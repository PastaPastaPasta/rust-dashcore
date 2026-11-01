//! LQ3a against Core: `qsigrec`, `clsig` and `isdlock` messages a v24 regtest network relayed,
//! each with the inventory hash Core announced for it (`tests/data/lq3a-regtest-captures.json`,
//! made by `contrib/node/lq3a-captures`). Every hash is ours. Every captured recovered signature
//! (each one Core's `quorum verify` confirmed at capture) is accepted here, against the tip's
//! quorums the capture lists, and a changed one is not. The capture holds `keepOldConnections`
//! quorums per type, and the oldest signed one of the recovered signatures (LQ3a D28).

use std::sync::OnceLock;

use dash_params::{ChainOption, ChainParamsBuilder, Network, Release};
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::core::BlsPkValue;
use dash_types::codec::BaseCodec;
use serde_json::Value;

use super::tests::{View, hex, rpc};
use super::*;

fn fixture() -> &'static Value {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        serde_json::from_str(include_str!("../../tests/data/lq3a-regtest-captures.json"))
            .expect("the capture")
    })
}

fn messages(command: &str) -> Vec<&'static Value> {
    let found: Vec<_> = fixture()["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter(|m| m["command"] == command)
        .collect();
    assert!(!found.is_empty(), "no {command}");
    found
}

fn payload(m: &Value) -> Vec<u8> {
    hex(m["payload"].as_str().expect("payload"))
}

/// The inventory hash Core announced, in wire order.
fn inv_hash(m: &Value) -> [u8; 32] {
    rpc(m["inv_hash"].as_str().expect("inv_hash"))
}

/// Regtest with the capture network's chain arguments (every node's are the same).
fn params() -> ChainSetup {
    let args = fixture()["provenance"]["extra_args"][0].as_array().expect("extra_args");
    let mut builder = ChainParamsBuilder::new(Network::Regtest, Release::V24_0);
    for arg in args {
        let (flag, value) = arg.as_str().expect("an arg").split_once('=').unwrap_or_default();
        let option = match flag {
            "-llmqtestinstantsenddip0024" => ChainOption::LlmqTestInstantSendDip0024,
            "-llmqtestparams" => ChainOption::LlmqTestParams,
            "-llmqtestinstantsendparams" => ChainOption::LlmqTestInstantSendParams,
            "-llmqtestplatformparams" => ChainOption::LlmqTestPlatformParams,
            // `-watchquorums` is the observer's role, not a chain parameter.
            _ => continue,
        };
        builder = builder.arg(option, value);
    }
    builder.build().expect("the capture's regtest")
}

/// The capture's quorums of `llmq_type` at the tip, newest first, verified with Core's keys.
fn quorums(llmq_type: LlmqType) -> Vec<ScannedQuorum> {
    fixture()["quorums"][llmq_type.to_base().to_string()]
        .as_array()
        .expect("the type's quorums")
        .iter()
        .map(|q| {
            let key = hex(q["public_key"].as_str().expect("a key"));
            let key = BlsPkValue::from_canonical(
                &key.try_into().expect("48 bytes"),
                BlsSchemeKind::Basic,
            )
            .expect("Core's key");
            let index = i16::try_from(q["index"].as_i64().expect("an index")).expect("small");
            let height = u32::try_from(q["height"].as_u64().expect("a height")).expect("small");
            ScannedQuorum::verified(
                BlockId(height),
                VerifiedQuorum::for_tests(
                    llmq_type,
                    QuorumHash::from(rpc(q["hash"].as_str().expect("a hash"))),
                    index,
                    key,
                ),
            )
        })
        .collect()
}

/// A view at the capture's tip holding `llmq_type`'s quorums: the active set and each by hash.
/// One type per view: the capture's two types share their quorum hashes.
fn view(llmq_type: LlmqType) -> View {
    let tip = u32::try_from(fixture()["tip_height"].as_u64().expect("tip")).expect("small");
    View::with_recent(tip, llmq_type, quorums(llmq_type))
}

fn recovered_sig(m: &Value) -> RecoveredSig {
    let bytes = payload(m);
    let mut r = &bytes[..];
    let rec = RecoveredSig::decode(&mut r).expect("Core's qsigrec");
    assert!(r.is_empty(), "the payload is one recovered sig: {m}");
    rec
}

fn recovered_sigs() -> Vec<RecoveredSig> {
    messages("qsigrec").into_iter().map(recovered_sig).collect()
}

fn clsig(m: &Value) -> ClSig {
    ClSig::decode(&mut &payload(m)[..]).expect("Core's clsig")
}

fn isdlock(m: &Value) -> IsdLock {
    IsdLock::decode(&mut &payload(m)[..]).expect("Core's isdlock")
}

#[test]
fn the_capture_is_cores_v24_regtest_in_the_basic_scheme() {
    let f = fixture();
    assert_eq!(f["provenance"]["core_tag"], "v24.0.0-rc.3");
    assert_eq!(f["network"], "regtest");
    assert_eq!(f["bls_scheme"], "basic");
    // V19 is buried at height 1, so the flag is basic from the genesis tip on.
    let v19 = &f["provenance"]["v19_deployment"];
    assert_eq!((&v19["active"], &v19["height"]), (&Value::Bool(true), &Value::from(1)));
    assert_eq!(f["provenance"]["dashd_sha256"].as_str().map(str::len), Some(64));
}

#[test]
fn every_inventory_hash_is_cores() {
    for m in messages("qsigrec") {
        assert_eq!(recovered_sig(m).hash(), inv_hash(m), "{m}");
    }
    for m in messages("clsig") {
        assert_eq!(
            clsig_hash(&clsig(m), ClsigHashSchemes::both(BlsCtx::BASIC)),
            Some(inv_hash(m)),
            "{m}"
        );
    }
    for m in messages("isdlock") {
        let islock = isdlock(m);
        assert_eq!(isdlock_hash(&islock), inv_hash(m), "{m}");
        // Read on use under Core's scheme, the signature is valid.
        assert!(matches!(read_lazy_sig(islock.sig.as_bytes(), BlsCtx::BASIC), SigRead::Valid(_)));
    }
}

#[test]
fn every_captured_recovered_sig_is_accepted() {
    let params = params();
    for m in messages("qsigrec") {
        let rec = recovered_sig(m);
        let view = view(rec.llmq_type);
        let verdict = verify_qsigrec(&params, &view, LockSchemes::both(BlsCtx::BASIC), &rec);
        let Ok(RecSigVerdict::Accept(quorum)) = verdict else {
            panic!("{verdict:?}: {m}")
        };
        assert_eq!(quorum.quorum_hash(), &rec.quorum_hash);
        assert_eq!(serves_recovered_sig(&params, &view, &rec), Ok(true));
    }
    // Both types signed.
    let recs = recovered_sigs();
    for t in [LlmqType::LlmqTest, LlmqType::LlmqTestInstantsend] {
        assert!(recs.iter().any(|r| r.llmq_type == t), "no {t:?}");
    }
}

/// Core relayed a recovered signature by the third-newest `llmq_test` quorum: active, because
/// `IsQuorumActive` scans `keepOldConnections` quorums, but outside the newest
/// `signingActiveQuorumCount` that signing selects from (LQ3a D28).
#[test]
fn a_recovered_sig_of_the_oldest_active_quorum_is_accepted() {
    let params = params();
    let llmq = params.consensus.llmq(LlmqType::LlmqTest).expect("llmq_test");
    let keep = usize::from(llmq.keep_old_connections);
    assert!(keep > usize::from(llmq.signing_active_quorum_count));
    for t in [LlmqType::LlmqTest, LlmqType::LlmqTestInstantsend] {
        assert_eq!(quorums(t).len(), keep, "{t:?}");
    }
    let signed = &fixture()["provenance"]["keep_old_connections_recsig"];
    let oldest = QuorumHash::from(rpc(signed["quorum_hash"].as_str().expect("a hash")));
    assert_eq!(quorums(LlmqType::LlmqTest)[keep - 1].quorum_hash(), oldest);
    let rec = recovered_sigs()
        .into_iter()
        .find(|r| r.llmq_type == LlmqType::LlmqTest && r.quorum_hash == oldest)
        .expect("Core relayed it");
    assert_eq!(rec.id, rpc(signed["id"].as_str().expect("an id")));
    assert_eq!(rec.msg_hash, rpc(signed["msg_hash"].as_str().expect("a msg hash")));
    let verdict =
        verify_qsigrec(&params, &view(LlmqType::LlmqTest), LockSchemes::both(BlsCtx::BASIC), &rec);
    assert!(matches!(verdict, Ok(RecSigVerdict::Accept(_))), "{verdict:?}");
}

#[test]
fn a_changed_recovered_sig_is_not_accepted() {
    let params = params();
    for rec in recovered_sigs() {
        let view = view(rec.llmq_type);
        let verdict =
            |r: &RecoveredSig, bls| verify_qsigrec(&params, &view, LockSchemes::both(bls), r);
        let ban = |reason| Ok(RecSigVerdict::Ban(reason));
        // Another message or request id under the same signature.
        let mut other = rec.clone();
        other.msg_hash[0] ^= 1;
        assert_eq!(verdict(&other, BlsCtx::BASIC), ban(RecSigBan::InvalidSignature));
        let mut other = rec.clone();
        other.id[31] ^= 0x80;
        assert_eq!(verdict(&other, BlsCtx::BASIC), ban(RecSigBan::InvalidSignature));
        // Under the legacy flag Core's basic signature is never accepted: bytes the legacy scheme
        // does not read ban as malformed; the rest read as another signature, whose legacy check
        // is outside BS12's domain (no verdict, SB8 D12) or rejects.
        let legacy = verdict(&rec, BlsCtx::LEGACY);
        match read_lazy_sig(&rec.sig, BlsCtx::LEGACY) {
            SigRead::Invalid => assert_eq!(legacy, ban(RecSigBan::MalformedSignature)),
            _ => assert!(!matches!(legacy, Ok(RecSigVerdict::Accept(_))), "{legacy:?}"),
        }
        // Named for the other quorum of the type, the signature does not verify.
        let mut other = rec.clone();
        let hashes: Vec<_> = quorums(rec.llmq_type).iter().map(|q| q.quorum_hash()).collect();
        other.quorum_hash = *hashes.iter().find(|h| **h != rec.quorum_hash).expect("two quorums");
        assert_eq!(verdict(&other, BlsCtx::BASIC), ban(RecSigBan::InvalidSignature));
        // Named for a quorum outside the active set: ignored.
        other.quorum_hash = QuorumHash::from([0x5a; 32]);
        assert_eq!(
            verdict(&other, BlsCtx::BASIC),
            Ok(RecSigVerdict::Ignore(RecSigIgnore::InactiveQuorum))
        );
    }
}

/// Each lock Core relayed carries a recovered signature it also relayed: the ChainLock type's for
/// (`clsig`, height) over the block hash, the InstantSend type's for (`islock`, inputs) over the
/// txid. So each lock's signature is one this module accepts.
#[test]
fn each_lock_carries_a_recovered_sig_core_relayed() {
    let params = params();
    let recs = recovered_sigs();
    let find = |llmq_type: LlmqType, id: [u8; 32], msg: &[u8; 32], sig: &[u8; 96]| {
        recs.iter()
            .find(|r| r.llmq_type == llmq_type && r.id == id && &r.msg_hash == msg && &r.sig == sig)
            .cloned()
    };
    for m in messages("clsig") {
        let clsig = clsig(m);
        let llmq_type = params.consensus.llmq_type_chain_locks;
        let rec = find(
            llmq_type,
            chainlock_request_id(clsig.height),
            clsig.block_hash.as_bytes(),
            clsig.sig.as_bytes(),
        )
        .unwrap_or_else(|| panic!("no recovered sig for {m}"));
        assert!(matches!(
            verify_qsigrec(&params, &view(llmq_type), LockSchemes::both(BlsCtx::BASIC), &rec),
            Ok(RecSigVerdict::Accept(_))
        ));
    }
    for m in messages("isdlock") {
        let islock = isdlock(m);
        let llmq_type = params.consensus.llmq_type_dip0024_instant_send;
        let rec = find(
            llmq_type,
            islock_request_id(&islock.inputs),
            islock.txid.as_bytes(),
            islock.sig.as_bytes(),
        )
        .unwrap_or_else(|| panic!("no recovered sig for {m}"));
        assert!(matches!(
            verify_qsigrec(&params, &view(llmq_type), LockSchemes::both(BlsCtx::BASIC), &rec),
            Ok(RecSigVerdict::Accept(_))
        ));
    }
}
