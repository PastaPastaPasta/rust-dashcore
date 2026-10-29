//! EV1-2's signed preimages (EV1 §3.18) against the oracle's `special_tx_sign_hash`, and its ECDSA
//! checks against Core's own signatures: the provider transactions of every type that Core
//! signed (BS6b's corpus, `dash-oracle-sys/tests/data/special-tx-signatures.json`).

use std::collections::BTreeMap;

use dash_consensus::hash::sha256d;
use dash_consensus::primitives::{Tx, decode_tx_exact};
use dash_dml::KeyId;
use dash_dml::specialtx::sig::{
    inputs_hash, make_sign_string, message_hash, payload_sig_hash, pro_dis_sign_hash,
    shared_reg_consent_hash, verify_hash, verify_hash_canonical, verify_message,
};
use dash_dml::testkit::replay::mainnet;
use dash_oracle_sys::mnlist::SignHashes;
use dash_oracle_sys::{Chain, Oracle};
use dash_params::ChainParams;
use dash_primitives::{
    CompactSig, InputsHash, MnType, NetInfo, NetInfoV2, ProRegTx, ProUpServTx, SpecialPayload,
    TxHash,
};
use dash_types::codec::BaseCodec;

#[path = "support/special_tx_records.rs"]
mod support;

use support::{SignedTx, encode_payload, records, unhex};

/// The record's ECDSA signers, as key ids.
fn key_ids(r: &SignedTx) -> Vec<KeyId> {
    let ids = r.signer["key_ids"].as_array().map(Vec::as_slice).unwrap_or_default();
    ids.iter()
        .map(|id| {
            KeyId::from(
                <[u8; 20]>::try_from(unhex(id.as_str().expect("a key id"))).expect("20 bytes"),
            )
        })
        .collect()
}

/// The chain Core signed `r` on, and its parameters.
fn chain_of(r: &SignedTx) -> (Chain, &'static ChainParams) {
    match r.network.as_str() {
        "mainnet" => (Chain::main(), mainnet()),
        _ => (Chain::regtest(), node_l1::chain::build::regtest()),
    }
}

/// Whether one of `ids` made `sig` over `hash`, canonically.
fn signed_by(ids: &[KeyId], hash: &[u8; 32], sig: &[u8]) -> bool {
    ids.iter().any(|id| verify_hash_canonical(hash, id, sig))
}

/// Our preimages of `tx` against Core's (`label` names the case).
fn compare_preimages(tx: &Tx, params: &ChainParams, core: &SignHashes, label: &str) {
    let Some(Ok(decoded)) = tx.payload() else {
        panic!("{label}: payload")
    };
    let payload_hash = Some(core.payload_hash);
    match &decoded.payload {
        SpecialPayload::ProviderRegister(p) => {
            assert_eq!(payload_sig_hash(p), payload_hash, "{label}");
            assert_eq!(core.consent_hash.is_some(), !p.shares.is_empty(), "{label}");
            if let Some(consent) = core.consent_hash {
                assert_eq!(shared_reg_consent_hash(p, tx), Some(consent), "{label}");
            }
            if let Some((text, hash)) = &core.sign_message {
                assert_eq!(make_sign_string(p, params).as_ref(), Some(text), "{label}");
                assert_eq!(message_hash(text), *hash, "{label}");
            }
        }
        SpecialPayload::ProviderUpdateService(p) => {
            assert_eq!(payload_sig_hash(p), payload_hash, "{label}")
        }
        SpecialPayload::ProviderUpdateRevoke(p) => {
            assert_eq!(payload_sig_hash(p), payload_hash, "{label}")
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            assert_eq!(payload_sig_hash(p), payload_hash, "{label}")
        }
        SpecialPayload::ProviderUpdateShare(p) => {
            assert_eq!(payload_sig_hash(p), payload_hash, "{label}")
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            assert_eq!(payload_sig_hash(p), payload_hash, "{label}")
        }
        SpecialPayload::ProviderDissolve(p) => {
            let count = u8::try_from(p.sigs.len()).expect("at most 8 signatures");
            assert_eq!(Some(pro_dis_sign_hash(p, tx, count)), core.consent_hash, "{label}");
        }
        other => panic!("{label}: not a provider payload: {other:?}"),
    }
}

/// Our preimages of `tx` against Core's, and our ECDSA checks of its signatures against Core's
/// signers; returns how many signatures we verified.
fn check(r: &SignedTx, tx: &Tx, params: &ChainParams, core: &SignHashes) -> usize {
    let txid = &r.txid;
    compare_preimages(tx, params, core, txid);
    let Some(Ok(decoded)) = tx.payload() else {
        panic!("{txid}: payload")
    };
    let ids = key_ids(r);
    let all_signed = |hash: &[u8; 32], sigs: &[CompactSig]| {
        sigs.iter().all(|sig| signed_by(&ids, hash, sig.as_bytes()))
    };
    match &decoded.payload {
        SpecialPayload::ProviderRegister(p) => match (&core.sign_message, core.consent_hash) {
            (_, Some(consent)) => {
                // Each share owner's join signature.
                for (share, sig) in p.shares.iter().zip(&p.join_sigs) {
                    let owner = &share.key_id_owner;
                    assert!(verify_hash_canonical(&consent, owner, sig.as_bytes()), "{txid}");
                }
                p.join_sigs.len()
            }
            (Some((text, _)), None) => {
                assert!(ids.iter().any(|id| verify_message(id, &p.vch_sig, text)), "{txid}");
                1
            }
            (None, None) => 0,
        },
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            // `CheckHashSig`: `VerifyHash`, not the canonical check.
            assert!(ids.iter().any(|id| verify_hash(&core.payload_hash, id, &p.vch_sig)), "{txid}");
            1
        }
        SpecialPayload::ProviderUpdateShare(p) => {
            assert!(signed_by(&ids, &core.payload_hash, &p.vch_sig), "{txid}");
            1
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            assert!(all_signed(&core.payload_hash, &p.sigs), "{txid}");
            p.sigs.len()
        }
        SpecialPayload::ProviderDissolve(p) => {
            let hash = core.consent_hash.expect("ProDisTx's sign hash");
            assert!(all_signed(&hash, &p.sigs), "{txid}");
            p.sigs.len()
        }
        // BLS operator signatures: `dash-oracle-sys`'s own test verifies them.
        _ => 0,
    }
}

#[test]
fn preimages_and_signatures_match_core() {
    let records = records();
    for oracle in Oracle::all() {
        let mut checked = BTreeMap::<u16, usize>::new();
        let mut signatures = 0;
        for r in &records {
            let (chain, params) = chain_of(r);
            let raw = unhex(&r.raw);
            let core = oracle
                .special_tx_sign_hash(&chain, &raw)
                .unwrap_or_else(|e| panic!("{}: {e}", r.txid));
            let tx = decode_tx_exact(&raw).expect("Core's transaction").into_value();
            signatures += check(r, &tx, params, &core);
            *checked.entry(r.tx_type).or_default() += 1;
        }
        assert_eq!(checked.keys().copied().collect::<Vec<_>>(), [1, 2, 3, 4, 10, 11, 12]);
        assert_eq!(checked.values().sum::<usize>(), 117);
        assert!(signatures >= 50, "{signatures}");
    }
}

/// Every Core-built ProTx that carries an inputs hash carries `CalcTxInputsHash` of its own inputs
/// (`sig::inputs_hash`, EV1 §3.18), the hash Core's `CheckInputsHash` compares.
#[test]
fn inputs_hashes_match_core() {
    let mut checked = BTreeMap::<u16, usize>::new();
    for r in records() {
        let tx = r.tx();
        let Some(Ok(decoded)) = tx.payload() else {
            panic!("{}: Core's payload", r.txid);
        };
        let carried = match &decoded.payload {
            SpecialPayload::ProviderRegister(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateService(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateRegistrar(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateRevoke(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateShare(p) => p.inputs_hash,
            SpecialPayload::ProviderUpdateSharedRegistrar(p) => p.inputs_hash,
            _ => continue,
        };
        assert_eq!(carried.as_bytes(), &inputs_hash(&tx), "{}", r.txid);
        *checked.entry(r.tx_type).or_default() += 1;
    }
    assert_eq!(checked.keys().copied().collect::<Vec<_>>(), [1, 2, 3, 4, 11, 12]);
}

/// The payload with its last 96 bytes (the BLS signature of a ProUpServTx or ProUpRevTx) replaced
/// by `sig`.
fn with_sig(payload: &[u8], sig: &[u8]) -> Vec<u8> {
    let mut out = payload[..payload.len() - 96].to_vec();
    out.extend_from_slice(sig);
    out
}

/// D22: a payload signature decodes under the payload version's scheme, else the other one's
/// (an invalid signature that still decodes), else not at all (`bad-protx-payload`), whatever
/// the global flag (`bls.h:183-201`). Core's own operator signatures spliced across payload
/// versions, and bytes canonical under neither scheme, decode as `GetTxPayload` reads them.
#[test]
fn signature_decode_follows_core_across_schemes() {
    let records = records();
    let signed: Vec<(u16, Tx)> = records
        .iter()
        .filter(|r| matches!(r.tx_type, 2 | 4))
        .map(|r| (r.tx_type, r.tx()))
        .collect();
    let legacy_sigs: Vec<&[u8]> = signed
        .iter()
        .map(|(_, t)| t.extra_payload())
        .filter(|p| p[..2] == [1, 0])
        .map(|p| &p[p.len() - 96..])
        .collect();
    let basic_sigs: Vec<&[u8]> = signed
        .iter()
        .map(|(_, t)| t.extra_payload())
        .filter(|p| p[..2] != [1, 0])
        .map(|p| &p[p.len() - 96..])
        .collect();
    assert!(!legacy_sigs.is_empty() && !basic_sigs.is_empty());
    for oracle in Oracle::all() {
        let mut seen = BTreeMap::<(&str, bool), usize>::new();
        for (tx_type, tx) in &signed {
            let payload = tx.extra_payload();
            let legacy_version = payload[..2] == [1, 0];
            let other = if legacy_version {
                &basic_sigs
            } else {
                &legacy_sigs
            };
            let neither = [0xffu8; 96];
            for (case, sig) in [
                ("own", &payload[payload.len() - 96..]),
                ("other", other[0]),
                ("neither", &neither[..]),
            ] {
                let spliced = with_sig(payload, sig);
                let raw = {
                    let mut t = tx.to_mutable();
                    t.extra_payload = spliced.clone();
                    Tx::new(t)
                };
                let decoded = match raw.payload() {
                    Some(Ok(d)) if d.is_complete() => Some(&d.payload),
                    _ => None,
                };
                let ours = decoded.is_some();
                if let Some(p) = decoded {
                    let decoded_sig = match p {
                        SpecialPayload::ProviderUpdateService(p) => &p.sig,
                        SpecialPayload::ProviderUpdateRevoke(p) => &p.sig,
                        p => panic!("{p:?}"),
                    };
                    // A signature canonical only in the other scheme decodes to the invalid
                    // object, which no verification accepts (`bls.h:184-203`).
                    assert_eq!(decoded_sig.is_valid(), case == "own", "type {tx_type} {case}");
                }
                for bls_legacy in [false, true] {
                    let core = match oracle.payload_decode(*tx_type, bls_legacy, &spliced) {
                        Ok(d) => {
                            if let Some(p) = decoded {
                                assert_eq!(
                                    encode_payload(p),
                                    d.reencoded,
                                    "type {tx_type} {case}: Core's re-encoding"
                                );
                            }
                            // `Reset()`: Core re-encodes the invalid object, not the wire bytes.
                            if d.reencoded[d.reencoded.len() - 96..] != *sig {
                                *seen.entry(("reset", bls_legacy)).or_default() += 1;
                            }
                            d.consumed_all
                        }
                        Err(e) if e.kind == dash_oracle_sys::ErrorKind::Decode => false,
                        Err(e) => panic!("{case}: {e}"),
                    };
                    assert_eq!(ours, core, "type {tx_type} {case} (global legacy {bls_legacy})");
                }
                *seen.entry((case, ours)).or_default() += 1;
            }
        }
        // Each case occurs: a foreign-scheme signature decodes (as the invalid object, under either
        // global flag), one canonical in neither does not.
        for case in
            [("own", true), ("other", true), ("neither", false), ("reset", false), ("reset", true)]
        {
            assert!(seen.contains_key(&case), "{case:?}: {seen:?}");
        }
    }
}

/// An ExtNetInfo (version 1) with Core P2P entries `entries`, as wire bytes.
fn ext_wire(entries: &[&[u8]]) -> Vec<u8> {
    let mut wire = vec![1, 1, 0, entries.len() as u8];
    wire.extend(entries.iter().copied().flatten());
    wire
}

/// An IPv4 Core P2P entry (ADDRv2, the port big-endian).
fn ipv4_entry(ip: [u8; 4], port: u16) -> Vec<u8> {
    let mut out = vec![0x01, 1, 4];
    out.extend(ip);
    out.extend(port.to_be_bytes());
    out
}

/// D23: a signature signs the decoded payload's re-encoding, not its wire bytes. An entry Core
/// reads as invalid (an unknown type code, an address `IsValid` refuses) re-encodes as the
/// invalid type alone, so the hash of a ProUpServTx carrying one is Core's only from the decode.
/// (rc.4 refuses repeated and out-of-order purposes at decode, `netinfo.h:431`, so those forms
/// never reach a preimage.)
#[test]
fn preimages_hash_the_decoded_net_info_d23() {
    let records = records();
    let r =
        records.iter().find(|r| r.tx_type == 2 && r.network == "regtest").expect("a ProUpServTx");
    let tx = r.tx();
    let Some(Ok(decoded)) = tx.payload() else {
        panic!("payload")
    };
    let SpecialPayload::ProviderUpdateService(base) = &decoded.payload else {
        panic!("type 2")
    };
    let canonical = ext_wire(&[&ipv4_entry([10, 1, 2, 3], 9999)]);
    let mut p: ProUpServTx = base.clone();
    p.version = 3;
    p.mn_type = MnType::Regular;
    (p.platform_node_id, p.platform_p2p_port, p.platform_http_port) = (None, None, None);
    let mut rest = &canonical[..];
    p.net_info = NetInfo::Extended(NetInfoV2::decode(&mut rest).expect("an ExtNetInfo"));
    let mut payload = Vec::new();
    p.encode(&mut payload);
    let at = payload.windows(canonical.len()).position(|w| w == canonical).expect("the NetInfo");
    for oracle in Oracle::all() {
        for (case, entry) in [
            ("canonical", ipv4_entry([10, 1, 2, 3], 9999)),
            ("unknown type", vec![0x07]),
            ("invalid address", ipv4_entry([0, 0, 0, 0], 9999)),
        ] {
            let mut wire = payload[..at].to_vec();
            wire.extend(ext_wire(&[&entry]));
            wire.extend(&payload[at + canonical.len()..]);
            let mut t = tx.to_mutable();
            t.extra_payload = wire.clone();
            let mut raw = Vec::new();
            t.encode(&mut raw);
            let core = oracle
                .special_tx_sign_hash(&Chain::regtest(), &raw)
                .unwrap_or_else(|e| panic!("{case}: {e}"));
            let ours = decode_tx_exact(&raw).expect("decodes").into_value();
            let Some(Ok(d)) = ours.payload() else {
                panic!("{case}: our decode")
            };
            assert!(d.is_complete(), "{case}");
            let SpecialPayload::ProviderUpdateService(q) = &d.payload else {
                panic!("{case}: type 2")
            };
            assert_eq!(payload_sig_hash(q), Some(core.payload_hash), "{case}");
            // The contrast: the wire bytes, signature excluded.
            let spliced = sha256d(&wire[..wire.len() - 96]);
            assert_eq!(spliced == core.payload_hash, case == "canonical", "{case}: contrast");
        }
    }
}

/// D23 for ProRegTx: `MakeSignString` and the shared registration's consent hash carry the
/// payload hash of the decoded NetInfo too.
#[test]
fn registration_preimages_hash_the_decoded_net_info_d23() {
    let records = records();
    let canonical = ext_wire(&[&ipv4_entry([10, 1, 2, 3], 9999)]);
    let mut shapes = BTreeMap::<bool, usize>::new();
    for oracle in Oracle::all() {
        for r in records.iter().filter(|r| r.tx_type == 1) {
            let tx = r.tx();
            let Some(Ok(decoded)) = tx.payload() else {
                panic!("payload")
            };
            let SpecialPayload::ProviderRegister(base) = &decoded.payload else {
                panic!("type 1")
            };
            if base.mn_type != MnType::Regular {
                continue;
            }
            let mut p: ProRegTx = base.clone();
            if p.version < 3 {
                p.version = 3;
                p.payouts = vec![];
            }
            let mut rest = &canonical[..];
            p.net_info = NetInfo::Extended(NetInfoV2::decode(&mut rest).expect("an ExtNetInfo"));
            let mut payload = Vec::new();
            p.encode(&mut payload);
            let at =
                payload.windows(canonical.len()).position(|w| w == canonical).expect("the NetInfo");
            for (case, entry) in
                [("unknown type", vec![0x07]), ("invalid address", ipv4_entry([0, 0, 0, 0], 9999))]
            {
                let mut wire = payload[..at].to_vec();
                wire.extend(ext_wire(&[&entry]));
                wire.extend(&payload[at + canonical.len()..]);
                let mut t = tx.to_mutable();
                t.extra_payload = wire;
                let mut raw = Vec::new();
                t.encode(&mut raw);
                let txid = &r.txid;
                let core = oracle
                    .special_tx_sign_hash(&Chain::regtest(), &raw)
                    .unwrap_or_else(|e| panic!("{txid} {case}: {e}"));
                let ours = decode_tx_exact(&raw).expect("decodes").into_value();
                let Some(Ok(d)) = ours.payload() else {
                    panic!("{txid} {case}: our decode")
                };
                let SpecialPayload::ProviderRegister(q) = &d.payload else {
                    panic!("type 1")
                };
                assert_eq!(payload_sig_hash(q), Some(core.payload_hash), "{txid} {case}");
                if let Some((text, _)) = &core.sign_message {
                    assert_eq!(
                        make_sign_string(q, node_l1::chain::build::regtest()).as_ref(),
                        Some(text),
                        "{txid} {case}"
                    );
                }
                if let Some(consent) = core.consent_hash {
                    assert_eq!(shared_reg_consent_hash(q, &ours), Some(consent), "{txid} {case}");
                }
                *shapes.entry(!q.shares.is_empty()).or_default() += 1;
            }
        }
    }
    // Both shapes: an external collateral's sign string, a shared registration's consent hash.
    assert!(shapes.len() == 2, "{shapes:?}");
}

/// A deterministic xorshift generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        std::array::from_fn(|_| self.next() as u8)
    }

    fn script(&mut self) -> Vec<u8> {
        let len = self.below(40) as usize;
        (0..len).map(|_| self.next() as u8).collect()
    }
}

/// `p` with its signed fields drawn from `rng`, in the shape it has (version, shared or not).
fn mutate(p: &mut SpecialPayload, rng: &mut Rng) {
    let hash = |rng: &mut Rng| TxHash::from(rng.bytes::<32>());
    let key_id = |rng: &mut Rng| KeyId::from(rng.bytes::<20>());
    let inputs = |rng: &mut Rng| InputsHash::from(rng.bytes::<32>());
    match p {
        SpecialPayload::ProviderRegister(p) => {
            if p.shares.is_empty() && rng.below(2) == 0 {
                p.collateral_hash = hash(rng);
            }
            p.collateral_index = rng.next() as u32;
            p.key_id_voting = key_id(rng);
            p.operator_reward = rng.next() as u16;
            p.script_payout = rng.script().into();
            p.inputs_hash = inputs(rng);
            if p.shares.is_empty() {
                p.key_id_owner = key_id(rng);
            }
            for payout in &mut p.payouts {
                (payout.script_payout, payout.reward) = (rng.script().into(), rng.next() as u16);
            }
            if let Some(extra) = p.payouts.first().cloned().filter(|_| rng.below(2) == 0) {
                p.payouts.push(extra);
            }
            for share in &mut p.shares {
                share.amount = rng.next() as i64;
                (share.script_refund, share.script_reward) =
                    (rng.script().into(), rng.script().into());
                share.key_id_owner = key_id(rng);
            }
            if !p.shares.is_empty() {
                (p.early_period_blocks, p.early_penalty) = (rng.next() as u32, rng.next() as i64);
            }
        }
        SpecialPayload::ProviderUpdateService(p) => {
            p.pro_tx_hash = hash(rng);
            p.script_operator_payout = rng.script().into();
            p.inputs_hash = inputs(rng);
        }
        SpecialPayload::ProviderUpdateRegistrar(p) => {
            p.pro_tx_hash = hash(rng);
            p.key_id_voting = key_id(rng);
            p.script_payout = rng.script().into();
            for payout in &mut p.payouts {
                (payout.script_payout, payout.reward) = (rng.script().into(), rng.next() as u16);
            }
            p.inputs_hash = inputs(rng);
        }
        SpecialPayload::ProviderUpdateRevoke(p) => {
            p.pro_tx_hash = hash(rng);
            p.inputs_hash = inputs(rng);
        }
        SpecialPayload::ProviderDissolve(p) => {
            p.pro_tx_hash = hash(rng);
            p.actor_index = rng.next() as u16;
        }
        SpecialPayload::ProviderUpdateShare(p) => {
            p.pro_tx_hash = hash(rng);
            p.share_index = rng.next() as u16;
            p.script_reward = rng.script().into();
            p.inputs_hash = inputs(rng);
        }
        SpecialPayload::ProviderUpdateSharedRegistrar(p) => {
            p.pro_tx_hash = hash(rng);
            p.key_id_voting = key_id(rng);
            p.inputs_hash = inputs(rng);
        }
        other => panic!("not a provider payload: {other:?}"),
    }
}

/// Every preimage against the oracle's across 1,000 generated payloads per type (EV1 §6.1):
/// Core's signed transactions with their signed fields and lock time drawn at random.
#[test]
fn generated_preimages_match_core() {
    const PER_TYPE: usize = 1_000;
    let records = records();
    let mut rng = Rng(0x5eed_e712);
    for oracle in Oracle::all() {
        let mut compared = BTreeMap::<u16, usize>::new();
        while compared.len() < 7 || compared.values().any(|n| *n < PER_TYPE) {
            for r in &records {
                if compared.get(&r.tx_type).is_some_and(|n| *n >= PER_TYPE) {
                    continue;
                }
                let (chain, params) = chain_of(r);
                let tx = r.tx();
                let Some(Ok(decoded)) = tx.payload() else {
                    panic!("{}: payload", r.txid)
                };
                let mut payload = decoded.payload.clone();
                mutate(&mut payload, &mut rng);
                let mut t = tx.to_mutable();
                t.extra_payload = encode_payload(&payload);
                t.lock_time = rng.next() as u32;
                let mut raw = Vec::new();
                t.encode(&mut raw);
                let label = format!("{} #{}", r.txid, compared.get(&r.tx_type).unwrap_or(&0));
                let core = oracle
                    .special_tx_sign_hash(&chain, &raw)
                    .unwrap_or_else(|e| panic!("{label}: {e}"));
                let ours = decode_tx_exact(&raw).expect("decodes").into_value();
                compare_preimages(&ours, params, &core, &label);
                *compared.entry(r.tx_type).or_default() += 1;
            }
        }
    }
}
