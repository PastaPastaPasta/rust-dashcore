//! L0 vectors of the ECDSA checks (EV1 §2.5) and the verifier double.

use dash_consensus::primitives::decode_tx_exact;
use dash_core_vectors::cases::BlsScheme;
use dash_evo::spork::{key_id_of, sign_compact};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{PayoutShare, SpecialPayload, TxType};
use secp256k1::{PublicKey, SecretKey};

use super::*;
use crate::specialtx::trivial::tests::{pro_reg, pro_up_serv};

/// secp256k1's group order, big-endian.
const ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

fn signer(seed: u8, compressed: bool) -> (SecretKey, KeyId) {
    let key = SecretKey::from_secret_bytes([seed; 32]).expect("a key");
    let public = PublicKey::from_secret_key(&key);
    (key, KeyId::from(key_id_of(&public, compressed)))
}

/// The same signature with S negated (`n - s`) and the recovery id's parity flipped: it recovers
/// the same key.
fn high_s(sig: &[u8]) -> Vec<u8> {
    let mut out = sig.to_vec();
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let d = i16::from(ORDER[i]) - i16::from(sig[33 + i]) - borrow;
        out[33 + i] = d.rem_euclid(256) as u8;
        borrow = i16::from(d < 0);
    }
    out[0] = 27 + ((sig[0] - 27) ^ 1);
    out
}

#[test]
fn signatures_verify_against_their_key() {
    let hash = [7u8; 32];
    for compressed in [true, false] {
        let (key, id) = signer(1, compressed);
        let sig = sign_compact(&hash, &key, compressed);
        assert!(verify_hash(&hash, &id, &sig));
        assert!(verify_hash_canonical(&hash, &id, &sig));
        // Another key, hash or compression.
        assert!(!verify_hash(&hash, &signer(2, compressed).1, &sig));
        assert!(!verify_hash(&[8; 32], &id, &sig));
        assert!(!verify_hash(&hash, &signer(1, !compressed).1, &sig));
        // Not 65 bytes.
        assert!(!verify_hash(&hash, &id, &sig[..64]));
        assert!(!verify_hash_canonical(&hash, &id, &[sig.clone(), vec![0]].concat()));
    }
}

/// `RecoverCompact` masks the header, so 8 above it recovers the same key; the canonical check
/// takes 27 to 34 only.
#[test]
fn header_aliases_are_not_canonical() {
    let hash = [9u8; 32];
    let (key, id) = signer(3, true);
    let sig = sign_compact(&hash, &key, true);
    assert!((31..=34).contains(&sig[0]));
    let mut alias = sig.clone();
    alias[0] += 8;
    assert!(verify_hash(&hash, &id, &alias));
    assert!(!verify_hash_canonical(&hash, &id, &alias));
    for header in [0u8, 26, 35, 255] {
        alias[0] = header;
        assert!(!verify_hash_canonical(&hash, &id, &alias), "header {header}");
    }
}

/// A high-S signature recovers the key, so `VerifyHash` takes it; `VerifyHashCanonical` does not.
#[test]
fn high_s_is_not_canonical() {
    let hash = [11u8; 32];
    for compressed in [true, false] {
        let (key, id) = signer(4, compressed);
        let sig = sign_compact(&hash, &key, compressed);
        assert!(sig[33..] <= HALF_ORDER[..]);
        let malleated = high_s(&sig);
        assert!(malleated[33..] > HALF_ORDER[..]);
        assert!(verify_hash(&hash, &id, &malleated));
        assert!(!verify_hash_canonical(&hash, &id, &malleated));
    }
    // S at the half order is low.
    let (key, id) = signer(5, true);
    let mut sig = sign_compact(&hash, &key, true);
    sig[33..].copy_from_slice(&HALF_ORDER);
    assert_eq!(verify_hash_canonical(&hash, &id, &sig), verify_hash(&hash, &id, &sig));
}

#[test]
fn messages_sign_their_magic_hash() {
    let (key, id) = signer(6, true);
    let message = "XtB2b2pWzHzeGcRfGKYCZpvgvhQZBkEvxT|0|Xo…|Xo…|00ff";
    let sig = sign_compact(&message_hash(message), &key, true);
    assert!(verify_message(&id, &sig, message));
    assert!(!verify_message(&id, &sig, &message[1..]));
    // The hash of the text, not the text's own hash.
    assert!(!verify_hash(&sha256d(message.as_bytes()), &id, &sig));
}

#[test]
fn fixed_verifier_answers_by_scheme() {
    let verifier = FixedVerifier {
        basic: Some(true),
        legacy: None,
        ..FixedVerifier::default()
    };
    // Core's payloads: a ProUpServTx's signature and a ProRegTx's operator key.
    let (sig, pk) = (
        pro_up_serv(BlsScheme::Basic).sig().clone(),
        pro_reg(BlsScheme::Basic).pub_key_operator().clone(),
    );
    let msg = [0; 32];
    let (basic, legacy) = (VerifyScheme::after_v19(true), VerifyScheme::after_v19(false));
    assert_eq!(verifier.bls_verify_insecure(&sig, &pk, &msg, basic), Ok(true));
    assert_eq!(verifier.bls_verify_insecure(&sig, &pk, &msg, legacy), Err(VerifyUnavailable));
    assert_eq!(verifier.calls.get(), 2);
}

/// The verify scheme is Core's global flag: legacy until V19 is active after the block's parent,
/// whatever scheme the payload's own key or signature decoded with.
#[test]
fn verify_scheme_follows_v19() {
    assert_eq!(VerifyScheme::after_v19(false).kind(), BlsSchemeKind::Legacy);
    assert_eq!(VerifyScheme::after_v19(true).kind(), BlsSchemeKind::Basic);
}

/// BS6b's Core-signed transactions (`tests/data/core_signed_protx.txt`): the transaction and its
/// signers' key ids (none listed when the payload names them: a shared registration's owners).
fn core_signed() -> Vec<(Tx, Vec<KeyId>)> {
    let text = include_str!("../../../tests/data/core_signed_protx.txt");
    let unhex = |s: &str| -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
            .collect()
    };
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .map(|line| {
            let [_, network, ids, raw] = line.split(' ').collect::<Vec<_>>()[..] else {
                panic!("a fixture line: {line}");
            };
            assert_eq!(network, "regtest", "the fixture's network");
            let ids = ids.split(',').filter(|id| *id != "-");
            let ids = ids.map(|id| KeyId::from(<[u8; 20]>::try_from(unhex(id)).expect("20 bytes")));
            let raw = unhex(raw);
            let tx = decode_tx_exact(&raw).expect("Core's transaction").into_value();
            (tx, ids.collect())
        })
        .collect()
}

/// Our preimages are the hashes Core's signers signed (EV1 §3.18): each ECDSA signature of a
/// Core-signed ProRegTx (external collateral message, shared registration consent), ProUpRegTx,
/// ProDisTx, ProUpShareTx and ProUpSharedRegTx verifies over ours.
#[test]
fn core_signatures_verify_over_our_preimages() {
    let params = dash_params::ChainParamsBuilder::new(
        dash_params::Network::Regtest,
        dash_params::Release::V24_0,
    )
    .build()
    .expect("Core's parameters");
    let signed_by = |ids: &[KeyId], hash: &[u8; 32], sig: &[u8]| {
        ids.iter().any(|id| verify_hash_canonical(hash, id, sig))
    };
    let mut verified = std::collections::BTreeMap::<&str, usize>::new();
    for (tx, ids) in core_signed() {
        let Some(Ok(decoded)) = tx.payload() else {
            panic!("{}: Core's payload", tx.txid());
        };
        let txid = tx.txid();
        let kind = match decoded.payload() {
            SpecialPayload::ProviderRegister(p) if !p.shares().is_empty() => {
                let consent = shared_reg_consent_hash(p, &tx).expect("a consent hash");
                for (share, sig) in p.shares().iter().zip(p.join_sigs()) {
                    assert!(verify_hash_canonical(&consent, &share.key_id_owner(), sig.as_bytes()));
                }
                "shared registration"
            }
            SpecialPayload::ProviderRegister(p) => {
                let text = make_sign_string(p, &params).expect("a sign string");
                assert!(
                    ids.iter().any(|id| verify_message(id, p.sig().as_slice(), &text)),
                    "{txid}"
                );
                "registration message"
            }
            SpecialPayload::ProviderUpdateRegistrar(p) => {
                let hash = payload_sig_hash(p).expect("a hash");
                assert!(ids.iter().any(|id| verify_hash(&hash, id, p.sig().as_slice())), "{txid}");
                "registrar update"
            }
            SpecialPayload::ProviderDissolve(p) => {
                let count = u8::try_from(p.sigs().len()).expect("at most 8 signatures");
                let hash = pro_dis_sign_hash(p, &tx, count);
                assert!(p.sigs().iter().all(|s| signed_by(&ids, &hash, s.as_bytes())), "{txid}");
                "dissolution"
            }
            SpecialPayload::ProviderUpdateShare(p) => {
                let hash = payload_sig_hash(p).expect("a hash");
                assert!(signed_by(&ids, &hash, p.sig()), "{txid}");
                "share update"
            }
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
                let hash = payload_sig_hash(p).expect("a hash");
                assert!(p.sigs().iter().all(|s| signed_by(&ids, &hash, s.as_bytes())), "{txid}");
                "shared registrar update"
            }
            other => panic!("{txid}: not an ECDSA-signed payload: {other:?}"),
        };
        *verified.entry(kind).or_default() += 1;
    }
    assert_eq!(verified.len(), 6, "every signer kind: {verified:?}");
    assert!(verified.values().all(|n| *n == 2), "{verified:?}");
}

/// Register quirk 27 (EV1-4): `MakeSignString` spells the payout, owner and voting addresses with
/// the network's base58 prefixes (`providertx.cpp` rc.4:370-389), and the external collateral's
/// key signs it as a message under the "DarkCoin Signed Message" magic (`message.cpp` rc.4:25).
/// Core's strings and hashes (`tests/data/core_sign_strings.txt`, from the oracle) for each
/// Core-signed external-collateral ProRegTx on mainnet, testnet and regtest are ours, over one
/// payload hash: mainnet's string is another than testnet's, which regtest shares (the same
/// prefixes). The oracle re-checks it live
/// (`node-l1/tests/l1_specialtx_sig.rs::sign_string_per_network_quirk_27`).
#[test]
fn quirk_27_make_sign_string() {
    let regtest = dash_params::ChainParamsBuilder::new(
        dash_params::Network::Regtest,
        dash_params::Release::V24_0,
    )
    .build()
    .expect("Core's parameters");
    let params = |network: &str| match network {
        "mainnet" => crate::testkit::replay::mainnet(),
        "testnet" => crate::testkit::replay::testnet(),
        "regtest" => &regtest,
        other => panic!("a network: {other}"),
    };
    let text = include_str!("../../../tests/data/core_sign_strings.txt");
    let core: Vec<[&str; 4]> = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|line| {
            let mut fields = line.splitn(4, ' ');
            std::array::from_fn(|_| fields.next().expect("four fields"))
        })
        .collect();
    let mut checked = 0;
    for (tx, _) in core_signed() {
        let Some(Ok(decoded)) = tx.payload() else {
            continue;
        };
        let SpecialPayload::ProviderRegister(p) = decoded.payload() else {
            continue;
        };
        if p.collateral_hash().is_null() {
            continue;
        }
        let txid = tx.txid().to_string();
        let rows: Vec<_> = core.iter().filter(|[id, ..]| *id == txid).collect();
        assert_eq!(rows.len(), 3, "{txid}: one row per network");
        let mut strings = std::collections::BTreeMap::new();
        for [_, network, core_hash, core_text] in rows {
            let ours = make_sign_string(p, params(network)).expect("a sign string");
            assert_eq!(ours, *core_text, "{txid} {network}");
            let hash: String = message_hash(&ours).iter().map(|b| format!("{b:02x}")).collect();
            assert_eq!(hash, *core_hash, "{txid} {network}");
            // `CMessageSigner`'s hash: the magic, then the string, each after its length.
            let magic = b"DarkCoin Signed Message:\n";
            let mut preimage = vec![u8::try_from(magic.len()).expect("a short magic")];
            preimage.extend_from_slice(magic);
            preimage.push(u8::try_from(ours.len()).expect("a sign string under 253 bytes"));
            preimage.extend_from_slice(ours.as_bytes());
            assert_eq!(message_hash(&ours), sha256d(&preimage), "{txid} {network}");
            strings.insert(*network, ours);
        }
        let payload_hashes: std::collections::BTreeSet<_> =
            strings.values().map(|s| s.rsplit('|').next().expect("a field").to_owned()).collect();
        assert_eq!(payload_hashes.len(), 1, "{strings:?}");
        assert_ne!(strings["mainnet"], strings["testnet"]);
        assert_eq!(strings["testnet"], strings["regtest"]);
        checked += 1;
    }
    assert_eq!(checked, 2, "every external-collateral ProRegTx");
}

/// The inputs hash a Core-signed ProTx carries is ours (`CalcTxInputsHash`, EV1 §3.18).
#[test]
fn core_inputs_hashes_are_ours() {
    let mut checked = 0;
    for (tx, _) in core_signed() {
        let Some(Ok(decoded)) = tx.payload() else {
            panic!("{}: Core's payload", tx.txid());
        };
        let carried = match decoded.payload() {
            SpecialPayload::ProviderRegister(p) => p.inputs_hash(),
            SpecialPayload::ProviderUpdateService(p) => p.inputs_hash(),
            SpecialPayload::ProviderUpdateRegistrar(p) => p.inputs_hash(),
            SpecialPayload::ProviderUpdateRevoke(p) => p.inputs_hash(),
            SpecialPayload::ProviderUpdateShare(p) => p.inputs_hash(),
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => p.inputs_hash(),
            _ => continue,
        };
        assert_eq!(carried.as_bytes(), &inputs_hash(&tx), "{}", tx.txid());
        checked += 1;
    }
    // Two of the twelve are ProDisTx, which carries none.
    assert_eq!(checked, 10);
}

/// `CalcTxInputsHash` streams each prevout (hash, then the index little-endian) in input order,
/// with no count: no inputs hash the empty string, and swapping two inputs changes it.
#[test]
fn inputs_hash_streams_the_prevouts() {
    let (tx, _) = core_signed().remove(0);
    let mut raw = tx.to_mutable();
    let first = raw.inputs()[0].clone();
    let mut second = first.clone();
    second = dash_primitives::TxIn::new(
        dash_primitives::OutPoint::new(
            second.prevout().txid(),
            first.prevout().index().wrapping_add(1),
        ),
        second.script_sig().clone(),
        second.sequence(),
    );

    raw = dash_primitives::Transaction::new(
        raw.version(),
        raw.tx_type(),
        vec![],
        raw.outputs().clone(),
        raw.lock_time(),
        raw.extra_payload().clone(),
    );
    assert_eq!(inputs_hash(&Tx::new(raw.clone())), sha256d(&[]));

    raw = dash_primitives::Transaction::new(
        raw.version(),
        raw.tx_type(),
        vec![first.clone(), second.clone()],
        raw.outputs().clone(),
        raw.lock_time(),
        raw.extra_payload().clone(),
    );
    let mut stream = Vec::new();
    for input in [&first, &second] {
        stream.extend_from_slice(input.prevout().txid().as_bytes());
        stream.extend_from_slice(&input.prevout().index().to_le_bytes());
    }
    let ordered = inputs_hash(&Tx::new(raw.clone()));
    assert_eq!(ordered, sha256d(&stream));

    raw = dash_primitives::Transaction::new(
        raw.version(),
        raw.tx_type(),
        vec![second, first],
        raw.outputs().clone(),
        raw.lock_time(),
        raw.extra_payload().clone(),
    );
    assert_ne!(inputs_hash(&Tx::new(raw)), ordered);
}

/// Every corpus transaction with a provider payload the T6 sign-hash builders are checked over,
/// by source: the Core-signed set, the rows of Core's `trivially_valid.json` and
/// `trivially_invalid.json`, the blocks of the EV2b quirk vectors and of the two mainnet build
/// windows, and built version-3 registrations with a payout list.
fn t6_corpus() -> Vec<(&'static str, Tx)> {
    use dash_core_vectors::cases::load_trivial;
    let mut out: Vec<(&'static str, Tx)> =
        core_signed().into_iter().map(|(tx, _)| ("core_signed_protx", tx)).collect();
    for file in ["trivially_valid.json", "trivially_invalid.json"] {
        let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
        for case in load_trivial(file, &text).expect(file) {
            if let Ok(tx) = decode_tx_exact(&case.tx) {
                out.push((file, tx.into_value()));
            }
        }
    }
    let quirks = include_str!("../../../tests/data/ev2b-quirks.txt");
    for vector in crate::testkit::replay::read_vectors(quirks).expect("the quirk vectors") {
        let block = dash_consensus::primitives::decode_block(&mut &vector.block[..])
            .expect("the quirk block")
            .into_value();
        out.extend(block.txs().iter().map(|tx| ("ev2b-quirks", Tx::new(tx.raw().clone()))));
    }
    let windows: [&[u8]; 2] = [
        include_bytes!("../../../benches/data/mainnet-dml-1028160-1030159.bin"),
        include_bytes!("../../../benches/data/mainnet-dml-1371028-1373027.bin"),
    ];
    for bytes in windows {
        let window = crate::testkit::window::Window::decode(bytes).expect("the window");
        for block in window.blocks() {
            out.extend(block.txs().iter().map(|tx| ("mainnet-window", Tx::new(tx.raw().clone()))));
        }
    }
    out.extend(built_regs().iter().map(|(source, p)| {
        let tx = crate::specialtx::testctx::special(TxType::ProviderRegister, vec![], vec![], p);
        (*source, tx)
    }));
    out
}

/// Version-3 registrations the fixtures lack: Regular and Evo paying a P2PKH, a P2SH and a
/// non-standard script distinct rewards, and one for each net info shape no fixture has (a
/// domain and an invalid entry, an extended info of an unknown version, and an info not of the
/// payload's version).
fn built_regs() -> Vec<(&'static str, ProRegTx)> {
    use crate::specialtx::trivial::tests::{p2pkh, p2sh, pro_reg_v3};
    use dash_primitives::{MnType, NIEntry, NIPurpose, NetInfo, NetInfoV2, PlatformNodeId};
    use dash_types::Numeric as _;
    let base = pro_reg_v3();
    let with = |mn_type, net_info: NetInfo, payouts: Vec<PayoutShare>, node_id| {
        ProRegTx::new(
            base.version(),
            mn_type,
            base.mode(),
            base.collateral_hash(),
            base.collateral_index(),
            net_info,
            base.key_id_owner(),
            base.pub_key_operator().clone(),
            base.key_id_voting(),
            base.operator_reward(),
            vec![].into(),
            payouts,
            base.shares().clone(),
            base.join_sigs().clone(),
            base.early_period_blocks(),
            base.early_penalty(),
            base.inputs_hash(),
            node_id,
            base.platform_p2p_port(),
            base.platform_http_port(),
        )
    };
    let payouts = vec![
        PayoutShare::new(p2pkh([0x51; 20]).into(), 5000),
        PayoutShare::new(p2sh([0x52; 20]).into(), 3000),
        PayoutShare::new(vec![0x51, 0x52, 0x93].into(), 2000),
    ];
    let node_id = Some(PlatformNodeId::from([0x77; 20]));
    let entries = vec![
        NIEntry::Domain {
            name: b"node.example.org".to_vec(),
            port: 9999,
        },
        NIEntry::Invalid,
    ];
    let net_infos = [
        NetInfo::Extended(NetInfoV2::new(1, vec![(NIPurpose::from_base(0), entries)])),
        NetInfo::Extended(NetInfoV2::new(2, Vec::new())),
        crate::specialtx::trivial::tests::pro_reg(BlsScheme::Basic).net_info().clone(),
    ];
    let mut out = vec![
        ("payout-list", with(MnType::Regular, base.net_info().clone(), payouts.clone(), None)),
        ("payout-list", with(MnType::Evo, base.net_info().clone(), payouts, node_id)),
    ];
    out.extend(net_infos.into_iter().map(|n| ("net-info", with(MnType::Regular, n, vec![], None))));
    out
}

fn t6_networks() -> [(&'static str, &'static dash_params::ChainSetup); 3] {
    static REGTEST: std::sync::OnceLock<dash_params::ChainSetup> = std::sync::OnceLock::new();
    let regtest = REGTEST.get_or_init(|| {
        dash_params::ChainParamsBuilder::new(
            dash_params::Network::Regtest,
            dash_params::Release::V24_0,
        )
        .build()
        .expect("Core's parameters")
    });
    [
        ("mainnet", crate::testkit::replay::mainnet()),
        ("testnet", crate::testkit::replay::testnet()),
        ("regtest", regtest),
    ]
}

/// `p`'s node sign-hash bytes and hash are base-sdk's `sig_hash_bytes` and its SHA256d.
fn same_sig_hash<T: SignedPayload>(p: &T, fork_bytes: Vec<u8>, what: &str) {
    assert_eq!(SignedPayload::sig_hash_bytes(p).as_deref(), Some(&fork_bytes[..]), "{what}");
    same_hash(p, &fork_bytes, what);
}

/// `p`'s node sign hash, from base-sdk's `Signable::payload`, is the SHA256d of base-sdk's
/// `sig_hash_bytes`.
fn same_hash<T: SignedPayload>(p: &T, fork_bytes: &[u8], what: &str) {
    assert_eq!(payload_sig_hash(p), Some(sha256d(fork_bytes)), "{what}");
}

/// T6 §3.3: each node sign-hash builder equals the base-sdk helper it replaces, over every corpus
/// payload: the payload hashes, the ProRegTx message hash on every network and the shared
/// registration consent hash.
#[test]
fn t6_builders_equal_the_fork_helpers() {
    let networks = t6_networks();
    let mut checked = std::collections::BTreeMap::<&str, usize>::new();
    let mut branches = std::collections::BTreeSet::new();
    for (source, tx) in t6_corpus() {
        let Some(Ok(decoded)) = tx.payload() else {
            continue;
        };
        let what = format!("{source} {}", tx.txid());
        let kind = match decoded.payload() {
            SpecialPayload::ProviderRegister(p) => {
                branches.extend(pro_reg_branches(p));
                same_sig_hash(p, p.sig_hash_bytes(), &what);
                for (network, params) in &networks {
                    let fork = message_hash(&p.sign_string(&params.base_sdk_addr_params()));
                    assert_eq!(pro_reg_message_hash(p, params), Some(fork), "{what} {network}");
                }
                let fork = sha256d(&p.shared_consent_bytes(tx.raw()));
                let side_car = decoded.operator_key().expect("registration operator key");
                assert_eq!(
                    shared_reg_consent_hash_with_key(p, side_car, &tx),
                    Some(fork),
                    "{what}"
                );
                assert_eq!(shared_reg_consent_hash(p, &tx), Some(fork), "{what}");
                if p.shares().is_empty() {
                    "ProRegTx"
                } else {
                    "ProRegTx shared"
                }
            }
            SpecialPayload::ProviderUpdateRegistrar(p) => {
                same_hash(p, &p.sig_hash_bytes(), &what);
                "ProUpRegTx"
            }
            SpecialPayload::ProviderUpdateService(p) => {
                same_hash(p, &p.sig_hash_bytes(), &what);
                "ProUpServTx"
            }
            SpecialPayload::ProviderUpdateRevoke(p) => {
                same_hash(p, &p.sig_hash_bytes(), &what);
                "ProUpRevTx"
            }
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
                same_sig_hash(p, p.sig_hash_bytes(), &what);
                "ProUpSharedRegTx"
            }
            _ => continue,
        };
        *checked.entry(kind).or_default() += 1;
    }
    assert_eq!(checked.len(), 6, "every builder: {checked:?}");
    // A net info not of the payload's version does not survive a transaction's decode, so the
    // built payloads are also checked as built.
    let tx = crate::specialtx::testctx::tx_of(TxType::ProviderRegister, vec![], vec![], vec![]);
    for (source, p) in built_regs() {
        branches.extend(pro_reg_branches(&p));
        same_sig_hash(&p, p.sig_hash_bytes(), source);
        let fork = sha256d(&p.shared_consent_bytes(tx.raw()));
        assert_eq!(shared_reg_consent_hash(&p, &tx), Some(fork), "{source}");
    }
    let missing: Vec<_> = PRO_REG_BRANCHES.iter().filter(|b| !branches.contains(**b)).collect();
    assert!(missing.is_empty(), "ProRegTx builder branches not in the corpus: {missing:?}");
}

/// Every branch of the ProRegTx sign-hash and consent writers: each payload version, the Evo
/// fields with and without ports, internal and shared registrations, and each net info shape.
const PRO_REG_BRANCHES: [&str; 13] = [
    "v1 Regular",
    "v2 Regular",
    "v2 Evo",
    "v3 Regular",
    "v3 Evo",
    "shares",
    "no shares",
    "net info legacy",
    "net info extended service",
    "net info extended domain",
    "net info extended invalid",
    "net info extended unsupported version",
    "net info not of the payload version",
];

/// The [`PRO_REG_BRANCHES`] `p` takes.
fn pro_reg_branches(p: &ProRegTx) -> Vec<&'static str> {
    use dash_primitives::{MnType, NIEntry, NetInfo, ProTxVersion};
    let evo = p.mn_type() == MnType::Evo;
    let mut out = vec![
        match (u16::from(p.version()), evo) {
            (1, false) => "v1 Regular",
            (2, false) => "v2 Regular",
            (2, true) => "v2 Evo",
            (3, false) => "v3 Regular",
            (3, true) => "v3 Evo",
            _ => "other version",
        },
        if p.shares().is_empty() {
            "no shares"
        } else {
            "shares"
        },
    ];
    let extended = p.version() >= ProTxVersion::CoreV24;
    match p.net_info() {
        NetInfo::Legacy(_) if !extended => out.push("net info legacy"),
        NetInfo::Extended(v2) if extended && v2.version() != 1 => {
            out.push("net info extended unsupported version");
        }
        NetInfo::Extended(v2) if extended => {
            for entry in v2.groups().iter().flat_map(|(_, group)| group) {
                out.push(match entry {
                    NIEntry::Service(_) => "net info extended service",
                    NIEntry::Domain {
                        ..
                    } => "net info extended domain",
                    NIEntry::Invalid => "net info extended invalid",
                });
            }
        }
        _ => out.push("net info not of the payload version"),
    }
    out
}

/// T6 §11.4: `make_sign_string` of every corpus ProRegTx on every network (the mainnet window
/// registrations on mainnet), pinned on base-sdk 29e01c83 (`test-vectors/t6/sign-strings.txt`).
#[test]
fn t6_sign_strings_are_pinned() {
    let networks = t6_networks();
    let mut rows = std::collections::BTreeSet::new();
    for (source, tx) in t6_corpus() {
        let Some(Ok(decoded)) = tx.payload() else {
            continue;
        };
        let SpecialPayload::ProviderRegister(p) = decoded.payload() else {
            continue;
        };
        for (network, params) in &networks {
            if source == "mainnet-window" && *network != "mainnet" {
                continue;
            }
            let text = make_sign_string(p, params).expect("a sign string");
            rows.insert(format!("{} {network} {text}\n", tx.txid()));
        }
    }
    assert!(rows.len() >= 3 * 4, "{} rows", rows.len());
    crate::t6_goldens::golden("sign-strings.txt", &rows.into_iter().collect::<String>());
}

/// The provider payloads' lists carry a one-byte count, which a CompactSize count matches only
/// below 253 entries: a version-3 ProRegTx with 253 and 256 payouts, shares and join signatures
/// (every list its sign hash and consent hash cover) equals the fork's helpers. A
/// ProUpSharedRegTx's sign hash covers no list (its signatures are left out).
#[test]
fn t6_builders_write_one_byte_list_counts() {
    use crate::specialtx::trivial::tests::{payout, pro_reg_shared};
    let base = pro_reg_shared();
    let join_sig: dash_primitives::JoinSig = crate::specialtx::testctx::compact(&[7; 65]);
    for n in [253, 256] {
        let p = ProRegTx::new(
            base.version(),
            base.mn_type(),
            base.mode(),
            base.collateral_hash(),
            base.collateral_index(),
            base.net_info().clone(),
            base.key_id_owner(),
            base.pub_key_operator().clone(),
            base.key_id_voting(),
            base.operator_reward(),
            base.script_payout().clone(),
            vec![payout(0x31, 1); n],
            vec![base.shares()[0].clone(); n],
            vec![join_sig; n],
            base.early_period_blocks(),
            base.early_penalty(),
            base.inputs_hash(),
            base.platform_node_id(),
            base.platform_p2p_port(),
            base.platform_http_port(),
        );
        same_sig_hash(&p, p.sig_hash_bytes(), &format!("{n} entries"));
        let tx = crate::specialtx::testctx::special(
            TxType::ProviderRegister,
            vec![crate::specialtx::testctx::txin([9; 32], 0)],
            Vec::new(),
            &p,
        );
        let fork = sha256d(&p.shared_consent_bytes(tx.raw()));
        assert_eq!(shared_reg_consent_hash(&p, &tx), Some(fork), "{n} entries");
    }
}
