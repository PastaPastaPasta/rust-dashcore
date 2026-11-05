//! L0 vectors of `validated_payload` (EV1 §6.1): Core's `evo_trivialvalidation` vectors, which run
//! `GetValidatedPayload<T>` against a regtest chain with V19 at height 100, and the payloads of
//! unknown version (D20).

use dash_consensus::primitives::{Tx, decode_tx_exact};
use dash_core_vectors::cases::{BlsScheme, TrivialCase, load_trivial};
use dash_primitives::{
    ProDisTx, ProRegTx, ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx,
    Transaction, TxType,
};

use super::*;
use crate::specialtx::reasons::{
    BAD_PRODIS_VERSION, BAD_PROTX_PAYLOAD, BAD_PROTX_REASON, BAD_PROTX_TYPE, BAD_PROTX_VERSION,
};

/// The `GetValidatedPayload` rows of `specialtxman.cpp` the vectors below reach.
pub(crate) const REACHED: &[(u32, TxValidationResult, &str)] = &[
    (1163, TxValidationResult::Consensus, BAD_PROTX_TYPE),
    (1169, TxValidationResult::Consensus, BAD_PROTX_PAYLOAD),
    (1174, TxValidationResult::Consensus, BAD_PROTX_VERSION),
];

/// `GetValidatedPayload<T>` for the type `tx` carries, as the runner switches on `nType`.
pub(crate) fn validate(tx: &Tx, v19: bool, v24: bool) -> Result<(), TxValidationError> {
    let (t, slot) = (tx.tx_type(), tx.payload());
    match t {
        TxType::ProviderRegister => validated_payload::<ProRegTx>(t, slot, v19, v24).map(drop),
        TxType::ProviderUpdateService => {
            validated_payload::<ProUpServTx>(t, slot, v19, v24).map(drop)
        }
        TxType::ProviderUpdateRegistrar => {
            validated_payload::<ProUpRegTx>(t, slot, v19, v24).map(drop)
        }
        TxType::ProviderUpdateRevoke => {
            validated_payload::<ProUpRevTx>(t, slot, v19, v24).map(drop)
        }
        TxType::ProviderDissolve => validated_payload::<ProDisTx>(t, slot, v19, v24).map(drop),
        TxType::ProviderUpdateShare => {
            validated_payload::<ProUpShareTx>(t, slot, v19, v24).map(drop)
        }
        TxType::ProviderUpdateSharedRegistrar => {
            validated_payload::<ProUpSharedRegTx>(t, slot, v19, v24).map(drop)
        }
        other => panic!("not a ProTx type: {other:?}"),
    }
}

fn core_cases(file: &'static str) -> Vec<TrivialCase> {
    let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
    load_trivial(file, &text).expect(file)
}

/// The runner's tip: height 100 (V19 active after it) for `basic`, 98 for `legacy`; V24 is not
/// active on that chain (`evo_trivialvalidation.cpp:39-49, 70-72`).
fn run_core_case(case: &TrivialCase) -> Result<(), TxValidationError> {
    let tx = decode_tx_exact(&case.tx).expect("Core's vector decodes").into_value();
    assert_eq!(*tx.txid().as_bytes(), case.tx_hash, "row {}: txid", case.index);
    validate(&tx, case.scheme == BlsScheme::Basic, false)
}

#[test]
fn core_trivially_valid_vectors() {
    let cases = core_cases("trivially_valid.json");
    assert!(!cases.is_empty());
    for case in &cases {
        assert_eq!(case.expected_error, None);
        assert_eq!(run_core_case(case), Ok(()), "row {} ({})", case.index, case.tx_type);
    }
}

#[test]
fn core_trivially_invalid_vectors() {
    let cases = core_cases("trivially_invalid.json");
    assert!(!cases.is_empty());
    for case in &cases {
        let want = case.expected_error.as_deref().expect("an expected error");
        let got = run_core_case(case).expect_err(want);
        assert_eq!(got.reject_reason(), want, "row {} ({})", case.index, case.tx_type);
    }
}

#[test]
fn version_max_follows_the_forks() {
    assert_eq!(version_max(false, false), 1);
    // V24 alone does not raise the maximum (`types.h:25-29`).
    assert_eq!(version_max(false, true), 1);
    assert_eq!(version_max(true, false), 2);
    assert_eq!(version_max(true, true), 3);
}

/// A ProTx of `tx_type` whose payload is `payload`.
pub(crate) fn pro_tx(tx_type: TxType, payload: Vec<u8>) -> Tx {
    Tx::new(Transaction {
        version: 3,
        tx_type,
        inputs: vec![],
        outputs: vec![],
        lock_time: 0,
        extra_payload: payload,
    })
}

fn rejects(tx: &Tx, v19: bool, v24: bool) -> (TxValidationResult, String) {
    let e = validate(tx, v19, v24).expect_err("rejected");
    (e.result(), e.reject_reason().to_owned())
}

/// D20: a provider payload of which Core reads only the version (0, or above 3) is
/// `bad-protx-version` when it is the whole payload, at the version step (above the maximum) or
/// at `IsTriviallyValid`'s first (version 0), and `bad-protx-payload` with any byte after it.
#[test]
fn unknown_versions_d20() {
    for tx_type in [
        TxType::ProviderRegister,
        TxType::ProviderUpdateService,
        TxType::ProviderUpdateRegistrar,
        TxType::ProviderUpdateRevoke,
    ] {
        for version in [0u16, 4, 0xffff] {
            for (v19, v24) in [(false, false), (true, false), (true, true)] {
                let bare = pro_tx(tx_type, version.to_le_bytes().to_vec());
                assert_eq!(
                    rejects(&bare, v19, v24),
                    (TxValidationResult::Consensus, BAD_PROTX_VERSION.to_owned()),
                    "{tx_type:?} v{version}"
                );
                let mut trailing = version.to_le_bytes().to_vec();
                trailing.push(0);
                assert_eq!(
                    rejects(&pro_tx(tx_type, trailing), v19, v24),
                    (TxValidationResult::Consensus, BAD_PROTX_PAYLOAD.to_owned()),
                    "{tx_type:?} v{version} + 1 byte"
                );
            }
        }
    }
}

/// Types 10 to 12 always decode in full (no unknown-version form): version 2 passes the tip's
/// maximum under V24 and fails the type's own check, version 4 fails the maximum.
#[test]
#[allow(clippy::disallowed_methods, reason = "a test payload re-encoded at another version")]
fn shared_types_version_boundary() {
    let pro_dis = |version: u16| {
        let mut p = crate::specialtx::trivial::tests::pro_dis();
        p.version = version;
        let mut payload = Vec::new();
        dash_types::codec::BaseCodec::encode(&p, &mut payload);
        pro_tx(TxType::ProviderDissolve, payload)
    };
    assert_eq!(validate(&pro_dis(1), true, true), Ok(()));
    let (class, reason) = rejects(&pro_dis(2), true, true);
    assert_eq!((class, reason.as_str()), (TxValidationResult::Consensus, BAD_PRODIS_VERSION));
    assert_eq!(rejects(&pro_dis(4), true, true).1, BAD_PROTX_VERSION);
}

/// The V24 gate for types 1 to 4: a valid version-3 (ExtAddr) payload is `bad-protx-version`
/// until V24 is active, and V24 without V19 does not raise the maximum.
// Core: evo/specialtxman.cpp:1173-1174; evo/types.h:25-29
#[test]
#[allow(clippy::disallowed_methods, reason = "test payloads encoded into a transaction")]
fn ext_addr_needs_v24() {
    fn tx_of<T: dash_types::codec::BaseCodec>(tx_type: TxType, p: &T) -> Tx {
        let mut payload = Vec::new();
        dash_types::codec::BaseCodec::encode(p, &mut payload);
        pro_tx(tx_type, payload)
    }
    let (reg, serv, up_reg, rev) = crate::specialtx::trivial::tests::ext_addr_payloads();
    let txs = [
        tx_of(TxType::ProviderRegister, &reg),
        tx_of(TxType::ProviderUpdateService, &serv),
        tx_of(TxType::ProviderUpdateRegistrar, &up_reg),
        tx_of(TxType::ProviderUpdateRevoke, &rev),
    ];
    let version = (TxValidationResult::Consensus, BAD_PROTX_VERSION.to_owned());
    for tx in &txs {
        let t = tx.tx_type();
        assert_eq!(validate(tx, true, true), Ok(()), "{t:?} under V24");
        assert_eq!(rejects(tx, true, false), version, "{t:?} before V24");
        assert_eq!(rejects(tx, false, true), version, "{t:?} under V24 without V19");
    }
}

/// A revocation reason from wire bytes: Core's legacy ProUpRevTx with its `nReason` (the `u16`
/// after the version and the ProTx hash) rewritten. Up to `CHANGE_OF_KEYS` (3) passes, above it is
/// `bad-protx-reason`.
// Core: evo/providertx.h:388-393; evo/providertx.cpp:484-485
#[test]
fn revocation_reasons_from_wire() {
    let case = core_cases("trivially_valid.json")
        .into_iter()
        .find(|c| c.tx_type == "prouprevtx" && c.scheme == BlsScheme::Legacy)
        .expect("Core's legacy ProUpRevTx");
    let base = decode_tx_exact(&case.tx).unwrap().into_value();
    for reason in [0u16, 1, 2, 3, 4, 0xffff] {
        let mut t = base.to_mutable();
        t.extra_payload[34..36].copy_from_slice(&reason.to_le_bytes());
        let got = validate(&Tx::new(t), false, false).map_err(|e| e.reject_reason().to_owned());
        let want = if reason <= 3 {
            Ok(())
        } else {
            Err(BAD_PROTX_REASON.to_owned())
        };
        assert_eq!(got, want, "reason {reason}");
    }
}

/// `GetValidatedPayload`'s first two steps: the type, then a payload that decodes in full.
#[test]
fn type_and_payload_steps() {
    let tx = pro_tx(TxType::ProviderUpdateRevoke, vec![]);
    let e = validated_payload::<ProRegTx>(TxType::ProviderUpdateRevoke, tx.payload(), true, true)
        .expect_err("another type");
    assert_eq!((e.result(), e.reject_reason()), (TxValidationResult::Consensus, BAD_PROTX_TYPE));
    // An empty payload, a truncated one and a missing payload field.
    assert_eq!(rejects(&tx, true, true).1, BAD_PROTX_PAYLOAD);
    let truncated = pro_tx(TxType::ProviderUpdateRevoke, vec![2, 0, 1]);
    assert_eq!(rejects(&truncated, true, true).1, BAD_PROTX_PAYLOAD);
    let e = validated_payload::<ProUpRevTx>(TxType::ProviderUpdateRevoke, None, true, true)
        .expect_err("no payload field");
    assert_eq!(e.reject_reason(), BAD_PROTX_PAYLOAD);
}

/// Each valid Core vector with a version above the tip's maximum: `bad-protx-version` before the
/// trivial checks.
#[test]
fn version_above_the_tips_maximum() {
    for case in core_cases("trivially_valid.json") {
        let tx = decode_tx_exact(&case.tx).unwrap().into_value();
        let basic = case.scheme == BlsScheme::Basic;
        if basic {
            // Version 2 under a pre-V19 tip.
            assert_eq!(
                rejects(&tx, false, false),
                (TxValidationResult::Consensus, BAD_PROTX_VERSION.to_owned())
            );
        } else {
            assert_eq!(validate(&tx, false, false), Ok(()), "row {}", case.index);
        }
    }
}

/// [`BlockPayloads`] holds each transaction's own slot, in order.
#[test]
fn block_payloads_by_position() {
    let txs = [
        pro_tx(TxType::ProviderUpdateRevoke, vec![0, 0]),
        Tx::new(Transaction {
            version: 1,
            tx_type: TxType::Spend,
            inputs: vec![],
            outputs: vec![],
            lock_time: 0,
            extra_payload: vec![],
        }),
    ];
    let payloads = BlockPayloads::from_txs(&txs);
    assert_eq!(payloads.len(), 2);
    assert!(!payloads.is_empty());
    assert!(BlockPayloads::from_txs(&[]).is_empty());
    assert!(payloads.get(1).is_none());
    let (Some(Ok(a)), Some(Ok(b))) = (payloads.get(0), txs[0].payload()) else {
        panic!("decoded")
    };
    assert!(std::ptr::eq(a, b), "the transaction's cached decode");
}
