//! EV1-2's `validated_payload` against Core's `CheckSpecialTx` (EV1 §6.1): the unknown-version
//! payloads (D20) and Core's trivial-validation vectors with the operator key in each BLS
//! encoding, on Core's runner chain (regtest, V19 at height 100, the tip at 98 or 100;
//! `evo_trivialvalidation.cpp`), and BS6b's shared-type transactions with V24 inactive and active.

use std::collections::BTreeSet;

use dash_consensus::primitives::{Tx, decode_tx_exact};
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_core_vectors::cases::{BlsScheme, load_trivial};
use dash_dml::specialtx::reasons::*;
use dash_dml::specialtx::{ProTxPayload, validated_payload};
use dash_oracle_sys::{
    Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, MnList, Oracle, ResultCode,
    ThresholdState, Verdict,
};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{
    ProDisTx, ProRegTx, ProUpRegTx, ProUpRevTx, ProUpServTx, ProUpShareTx, ProUpSharedRegTx,
    SpecialPayload, Transaction, TxType,
};
use dash_types::codec::BaseCodec;
use node_l1::dml_checks::{PRO_REG, PRO_UP_REG, PRO_UP_REV, PRO_UP_SERV, Table};

#[path = "support/special_tx_records.rs"]
mod support;

use support::{encode_payload, records, unhex};

/// `GetValidatedPayload<T>` for the type `tx` carries.
fn ours(tx: &Tx, v19: bool, v24: bool) -> Result<(), TxValidationError> {
    fn gvp<T: ProTxPayload>(tx: &Tx, v19: bool, v24: bool) -> Result<(), TxValidationError> {
        validated_payload::<T>(tx.tx_type(), tx.payload(), v19, v24).map(drop)
    }
    match tx.tx_type() {
        TxType::ProviderRegister => gvp::<ProRegTx>(tx, v19, v24),
        TxType::ProviderUpdateService => gvp::<ProUpServTx>(tx, v19, v24),
        TxType::ProviderUpdateRegistrar => gvp::<ProUpRegTx>(tx, v19, v24),
        TxType::ProviderUpdateRevoke => gvp::<ProUpRevTx>(tx, v19, v24),
        TxType::ProviderDissolve => gvp::<ProDisTx>(tx, v19, v24),
        TxType::ProviderUpdateShare => gvp::<ProUpShareTx>(tx, v19, v24),
        TxType::ProviderUpdateSharedRegistrar => gvp::<ProUpSharedRegTx>(tx, v19, v24),
        other => panic!("{other:?}"),
    }
}

/// The reasons `GetValidatedPayload` and `IsTriviallyValid` give for `tx_type`: the table's `GVP`
/// and `T` steps.
fn trivial_reasons(tx_type: TxType) -> BTreeSet<&'static str> {
    let table: &Table = match tx_type {
        TxType::ProviderRegister => &PRO_REG,
        TxType::ProviderUpdateService => &PRO_UP_SERV,
        TxType::ProviderUpdateRegistrar => &PRO_UP_REG,
        _ => &PRO_UP_REV,
    };
    let steps = table.steps.iter().filter(|s| s.id.starts_with("GVP") || s.id.starts_with('T'));
    steps.flat_map(|s| s.reasons.iter().copied()).collect()
}

/// A regtest chain of `n` headers from height 0 (only linkage matters to the context).
fn regtest_headers(oracle: &Oracle, n: usize) -> Vec<[u8; 80]> {
    let mut out = Vec::new();
    let mut prev = [0u8; 32];
    for i in 0..n {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&4i32.to_le_bytes());
        h[4..36].copy_from_slice(&prev);
        h[68..72].copy_from_slice(&(1_417_713_337 + 600 * i as u32).to_le_bytes());
        h[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
        prev = oracle.header_decode(&h).expect("a header").hash;
        out.push(h);
    }
    out
}

/// Core's runner: V19 active after height 100; the tip at 100 (basic) or 98 (legacy).
struct Runner {
    chain: Chain,
    headers: Vec<[u8; 80]>,
}

impl Runner {
    fn new(oracle: &Oracle) -> Self {
        Runner {
            chain: Chain::new("regtest", &["-testactivationheight=v19@100", "-dip3params=2:2"]),
            headers: regtest_headers(oracle, 101),
        }
    }

    /// Core's rejection of `raw` (class and reason), if any (`check_sigs` off: the trivial steps
    /// come first).
    fn core(&self, oracle: &Oracle, raw: &[u8], basic: bool, v24: bool) -> Option<(i32, String)> {
        let tip = if basic {
            100
        } else {
            98
        };
        let empty = format!("{}ffffffff0000000000", "00".repeat(32));
        let list = unhex(&empty);
        let ctx = Context::new(
            0,
            self.headers[..=tip].to_vec(),
            !basic,
            vec![(
                DEPLOYMENT_V24,
                if v24 {
                    ThresholdState::Active
                } else {
                    ThresholdState::Defined
                },
            )],
            1_500_000_000,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            MnList::Supplied(list),
            vec![],
        );
        match oracle.check_special_tx(&self.chain, raw, &ctx, false).expect("a verdict") {
            Verdict::Accept => None,
            Verdict::Reject(r) => {
                let ResultCode::Tx(class) = r.result else {
                    panic!("a transaction result: {r:?}");
                };
                Some((class, r.reason))
            }
        }
    }
}

/// Ours against Core's: our rejection is Core's; where ours passes, Core rejects at none of the
/// trivial steps (it goes on to the contextual checks, which an empty list fails).
fn agree(oracle: &Oracle, runner: &Runner, tx: &Tx, basic: bool, what: &str) -> bool {
    let mut raw = Vec::new();
    tx.raw().encode(&mut raw);
    let core = runner.core(oracle, &raw, basic, false);
    match ours(tx, basic, false) {
        Err(e) => {
            assert_eq!(core, Some(verdict(&e)), "{what}");
            false
        }
        Ok(()) => {
            let trivial = trivial_reasons(tx.tx_type());
            let reason = core.as_ref().map(|(_, r)| r.as_str());
            assert!(reason.is_none_or(|r| !trivial.contains(r)), "{what}: Core {core:?}");
            true
        }
    }
}

/// Our rejection as Core reports it: the class and the reason.
fn verdict(e: &TxValidationError) -> (i32, String) {
    (e.result() as i32, e.reject_reason().to_string())
}

fn pro_tx(base: &Tx, tx_type: TxType, payload: Vec<u8>) -> Tx {
    let mut t: Transaction = base.to_mutable();
    t.tx_type = tx_type;
    t.extra_payload = payload;
    Tx::new(t)
}

fn core_vectors() -> Vec<(BlsScheme, Tx)> {
    ["trivially_valid.json", "trivially_invalid.json"]
        .iter()
        .flat_map(|file| {
            let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
            load_trivial(file, &text).expect(file)
        })
        .map(|case| (case.scheme, decode_tx_exact(&case.tx).expect("Core's vector").into_value()))
        .collect()
}

/// D20 through Core: types 1-4, versions 0, 4 and 0xffff, alone and with a trailing byte.
#[test]
fn unknown_versions_match_core_d20() {
    let (_, base) = core_vectors().into_iter().next().expect("a vector");
    for oracle in Oracle::all() {
        let runner = Runner::new(&oracle);
        for tx_type in [
            TxType::ProviderRegister,
            TxType::ProviderUpdateService,
            TxType::ProviderUpdateRegistrar,
            TxType::ProviderUpdateRevoke,
        ] {
            for version in [0u16, 4, 0xffff] {
                for trailing in [false, true] {
                    let mut payload = version.to_le_bytes().to_vec();
                    payload.extend(trailing.then_some(0));
                    let tx = pro_tx(&base, tx_type, payload);
                    for basic in [false, true] {
                        let what = format!("{tx_type:?} v{version} trailing {trailing}");
                        assert!(!agree(&oracle, &runner, &tx, basic, &what), "{what}: rejected");
                    }
                }
            }
        }
    }
}

/// Both BLS encodings (EV1 §6.1): each Core vector with an operator key, the key's bytes
/// re-encoded in the other scheme. Core reads the key under the payload version's scheme, so
/// the re-encoded key is another point or none at all.
#[test]
fn operator_keys_in_both_encodings_match_core() {
    let vectors = core_vectors();
    for oracle in Oracle::all() {
        let runner = Runner::new(&oracle);
        let mut passed = 0;
        let mut keys = 0;
        let mut swapped_rejected = 0;
        for (scheme, tx) in &vectors {
            let Some(Ok(decoded)) = tx.payload() else {
                continue;
            };
            let key = match &decoded.payload {
                SpecialPayload::ProviderRegister(p) => &p.pub_key_operator,
                SpecialPayload::ProviderUpdateRegistrar(p) => &p.pub_key_operator,
                _ => continue,
            };
            let wire = key.to_bytes();
            let other = match key.scheme() {
                BlsSchemeKind::Legacy => BlsSchemeKind::Basic,
                BlsSchemeKind::Basic => BlsSchemeKind::Legacy,
            };
            // On a copy, read first: re-encoding a key never read writes the unread object's
            // bytes, as Core's `CBLSLazyWrapper::ToBytes` does, and the transaction caches its key.
            let copy = key.clone();
            copy.get();
            let reencoded = copy.to_bytes_as(other);
            if reencoded == wire {
                continue;
            }
            let payload = tx.extra_payload();
            let at = payload.windows(48).position(|w| w == wire).expect("the key's bytes");
            let mut spliced = payload.to_vec();
            spliced[at..at + 48].copy_from_slice(&reencoded);
            let swapped = pro_tx(tx, tx.tx_type(), spliced);
            let basic = *scheme == BlsScheme::Basic;
            for (what, t) in [("as shipped", tx), ("other encoding", &swapped)] {
                let label = format!("{} {what}", tx.txid());
                match agree(&oracle, &runner, t, basic, &label) {
                    true => passed += 1,
                    false if what == "other encoding" => swapped_rejected += 1,
                    false => {}
                }
            }
            keys += 1;
        }
        assert!(keys >= 8 && passed >= 4, "{keys} keys, {passed} passed");
        assert!(swapped_rejected >= 1, "no re-encoded key was refused");
    }
}

/// BS6b's Core-signed transactions of the shared types (10 to 12).
fn shared_records() -> Vec<Tx> {
    records().iter().filter(|r| (10..=12).contains(&r.tx_type)).map(|r| r.tx()).collect()
}

/// `p` with each field `IsTriviallyValid` reads pushed past its bound, and every payload
/// version up to 4.
fn shared_variants(p: &SpecialPayload) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![("as shipped".to_string(), encode_payload(p))];
    for version in 0..=4 {
        let mut q = p.clone();
        match &mut q {
            SpecialPayload::ProviderDissolve(q) => q.version = version,
            SpecialPayload::ProviderUpdateShare(q) => q.version = version,
            SpecialPayload::ProviderUpdateSharedRegistrar(q) => q.version = version,
            _ => unreachable!(),
        }
        out.push((format!("v{version}"), encode_payload(&q)));
    }
    let mut edit = |what: &str, f: &dyn Fn(&mut SpecialPayload)| {
        let mut q = p.clone();
        f(&mut q);
        out.push((what.to_string(), encode_payload(&q)));
    };
    let sigs = |n: usize| {
        move |q: &mut SpecialPayload| {
            let sigs = match q {
                SpecialPayload::ProviderDissolve(q) => &mut q.sigs,
                SpecialPayload::ProviderUpdateSharedRegistrar(q) => &mut q.sigs,
                _ => unreachable!(),
            };
            let first = sigs[0];
            sigs.resize(n, first);
        }
    };
    match p {
        SpecialPayload::ProviderDissolve(_) | SpecialPayload::ProviderUpdateSharedRegistrar(_) => {
            for n in [0, 1, 8, 9] {
                edit(&format!("{n} sigs"), &sigs(n));
            }
        }
        SpecialPayload::ProviderUpdateShare(_) => {
            for n in [0, 64, 66] {
                edit(&format!("sig of {n} bytes"), &move |q| {
                    if let SpecialPayload::ProviderUpdateShare(q) = q {
                        q.vch_sig.resize(n, 0x1b);
                    }
                });
            }
            for (what, script) in [("empty payee", vec![]), ("OP_RETURN payee", vec![0x6a])] {
                edit(what, &move |q| {
                    if let SpecialPayload::ProviderUpdateShare(q) = q {
                        q.script_reward = script.clone().into();
                    }
                });
            }
        }
        _ => unreachable!(),
    }
    if let SpecialPayload::ProviderUpdateSharedRegistrar(q) = p {
        edit("null voting key", &|q| {
            if let SpecialPayload::ProviderUpdateSharedRegistrar(q) = q {
                q.key_id_voting = dash_dml::KeyId::from([0u8; 20]);
            }
        });
        // The other half of `bad-protx-key-null`: the operator key's bytes zeroed, the null key.
        let mut zeroed = encode_payload(p);
        let key = q.pub_key_operator.to_bytes();
        let at = zeroed.windows(48).position(|w| w == key).expect("the operator key's bytes");
        zeroed[at..at + 48].fill(0);
        out.push(("null operator key".to_string(), zeroed));
    }
    let shipped = out[0].1.clone();
    out.push(("trailing byte".to_string(), [shipped.as_slice(), &[0]].concat()));
    out.push(("truncated".to_string(), shipped[..shipped.len() - 1].to_vec()));
    out
}

/// Types 10 to 12 through Core with V24 inactive and active (validator r1 note 3): ours rejects
/// as Core does, and where ours passes, Core rejects at the first step after
/// `GetValidatedPayload`: `-too-early` without V24, else the empty list's `-hash`.
#[test]
fn shared_types_match_core_across_v24() {
    let txs = shared_records();
    assert_eq!(txs.len(), 24, "BS6b's shared-type records");
    for oracle in Oracle::all() {
        let runner = Runner::new(&oracle);
        let (mut passed, mut reasons) = (0, BTreeSet::new());
        for tx in &txs {
            let Some(Ok(decoded)) = tx.payload() else {
                panic!("{}: Core's payload", tx.txid());
            };
            let (too_early, no_mn) = match tx.tx_type() {
                TxType::ProviderDissolve => (BAD_PRODIS_TOO_EARLY, BAD_PRODIS_HASH),
                TxType::ProviderUpdateShare => (BAD_PROUPSHARE_TOO_EARLY, BAD_PROUPSHARE_HASH),
                _ => (BAD_PROUPSHAREDREG_TOO_EARLY, BAD_PROUPSHAREDREG_HASH),
            };
            for (what, payload) in shared_variants(&decoded.payload) {
                let variant = pro_tx(tx, tx.tx_type(), payload);
                let mut raw = Vec::new();
                variant.raw().encode(&mut raw);
                for basic in [false, true] {
                    for v24 in [false, true] {
                        let label = format!("{} {what} basic {basic} v24 {v24}", tx.txid());
                        let core = runner.core(&oracle, &raw, basic, v24);
                        match ours(&variant, basic, v24) {
                            Err(e) => {
                                assert_eq!(core, Some(verdict(&e)), "{label}");
                                reasons.insert(e.reject_reason().to_string());
                            }
                            Ok(()) => {
                                // Core: evo/specialtxman.cpp rc.4:1623-1700, :1849-1870
                                let next = if v24 {
                                    (TxValidationResult::BadSpecial, no_mn)
                                } else {
                                    (TxValidationResult::Consensus, too_early)
                                };
                                assert_eq!(core, Some((next.0 as i32, next.1.into())), "{label}");
                                passed += 1;
                            }
                        }
                    }
                }
            }
        }
        // Every `GetValidatedPayload` reason of types 10 to 12 but `bad-protx-type` (the
        // dispatcher picks the check by type) and `bad-protx-operator-pubkey`: Core reads type 12's
        // key as basic whatever the bytes (providertx.h:534), so the wire never yields a legacy key.
        let want = [
            BAD_PROTX_PAYLOAD,
            BAD_PROTX_VERSION,
            BAD_PROTX_KEY_NULL,
            BAD_PRODIS_VERSION,
            BAD_PRODIS_SIG_COUNT,
            BAD_PROUPSHARE_VERSION,
            BAD_PROUPSHARE_SIG_SIZE,
            BAD_PROUPSHARE_PAYEE,
            BAD_PROUPSHAREDREG_VERSION,
            BAD_PROUPSHAREDREG_SIG_COUNT,
        ];
        assert_eq!(reasons, want.map(String::from).into(), "the reasons reached");
        // As shipped and as v1, plus one and eight signatures for types 10 and 12, at each tip
        // and V24 state: (15 x 4 + 9 x 2) x 4.
        assert_eq!(passed, 312, "the variants ours passes");
    }
}
