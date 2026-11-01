//! L0 vectors of EV1-4's checks (EV1 §6.1): `CheckProUpServTx` (§3.4) and `CheckProUpRevTx`
//! (§3.6), each reject by a [`super::CASES`] row, and the operator's BLS signature through the
//! bound verifier under both global schemes.

use std::sync::Arc;

use dash_core_vectors::cases::BlsScheme;
use dash_pkc::bls::{
    BlsEagerSig, BlsLazyPk, BlsScChia, BlsScIetf, BlsSchemeKind, BlsSchemeSource, BlsSecretKey,
    BlsSigId,
};
use dash_primitives::{
    MnType as PayloadMnType, PlatformNodeId, ProTxVersion, ProUpRevTx, ProUpServTx, TxHash, TxType,
};

use super::super::network::tests::{ipv4_entry, mn_ipv4};
use super::super::sig::{FixedVerifier, payload_sig_hash};
use super::super::trivial::tests::{p2pkh, pro_up_rev, pro_up_serv};
use super::*;
use crate::testkit::replay::{mainnet, testnet};
use crate::testkit::{dmn_with, operator_key, pro_tx};

/// The masternode the updates name.
const UPDATED: u8 = 0x21;
/// Another masternode, holding what a test makes collide.
const OTHER: u8 = 0x22;

/// Masternode [`UPDATED`]: regular, version 2, operator key [`UPDATED`] (basic), edited by `f`.
fn updated(f: impl FnOnce(&mut Dmn, &mut MnCold)) -> Arc<Dmn> {
    mn(UPDATED, f)
}

/// Masternode [`UPDATED`] as an Evo of version 2 with mainnet's platform ports.
fn updated_evo(f: impl FnOnce(&mut Dmn, &mut MnCold)) -> Arc<Dmn> {
    dmn_with(UPDATED, MnType::Evo, |d, c| {
        c.version = 2;
        c.pub_key_operator = operator_key(UPDATED, BlsSchemeKind::Basic);
        f(d, c)
    })
}

fn update_inputs() -> Vec<TxIn> {
    vec![txin([0x35; 32], 0)]
}

/// The basic-scheme signature of `hash` by the secret of [`operator_key`]`(n, _)`.
fn basic_sig(n: u8, hash: &[u8; 32]) -> BlsEagerSig {
    let sk = BlsSecretKey::<BlsScIetf>::from_ikm(&[n; 32]).expect("key material");
    let bytes = sk.sign_with(hash, BlsSigId::Basic).to_bytes();
    BlsEagerSig::decode(&bytes, BlsSchemeSource::Version(BlsSchemeKind::Basic)).expect("a sig")
}

/// The legacy secret of seed `n`: its public key, read under the legacy scheme.
fn legacy_key(n: u8) -> BlsLazyPk {
    let sk = BlsSecretKey::<BlsScChia>::from_ikm(&[n; 32]).expect("key material");
    BlsLazyPk::from_bytes(
        &sk.public_key().to_bytes(),
        BlsSchemeSource::Version(BlsSchemeKind::Legacy),
    )
}

/// The legacy-scheme signature of `hash` by [`legacy_key`]`(n)`.
fn legacy_sig(n: u8, hash: &[u8; 32]) -> BlsEagerSig {
    let sk = BlsSecretKey::<BlsScChia>::from_ikm(&[n; 32]).expect("key material");
    let bytes = sk.sign_digest(hash).to_bytes();
    BlsEagerSig::decode(&bytes, BlsSchemeSource::Version(BlsSchemeKind::Legacy)).expect("a sig")
}

// --- ProUpServTx (§3.4) ---

/// A service update of [`UPDATED`] to Core's trivially valid basic payload, no operator payee.
fn serv() -> (Fixture, ProUpServTx) {
    let fx = Fixture::v24().with_mn(updated(|_, _| {}));
    let mut p = pro_up_serv(BlsScheme::Basic);
    p.pro_tx_hash = TxHash::from(*pro_tx(UPDATED).as_bytes());
    p.script_operator_payout = Vec::new().into();
    (fx, p)
}

fn serv_tx(p: &mut ProUpServTx) -> Tx {
    let inputs = update_inputs();
    p.inputs_hash = carried_inputs_hash(&inputs);
    special(TxType::ProviderUpdateService, inputs, vec![], p)
}

fn check_serv(fx: &Fixture, mut p: ProUpServTx) -> Verdict {
    verdict(fx.check(&serv_tx(&mut p)))
}

/// [`serv`] as an Evo at version 2 (V24 off, where Core admits it): mainnet's platform ports and
/// a platform node id.
fn serv_evo() -> (Fixture, ProUpServTx) {
    let (mut fx, mut p) = serv();
    fx.rules.v24 = false;
    let fx = replaced(fx, updated_evo(|_, _| {}));
    p.mn_type = PayloadMnType::Evo;
    p.platform_node_id = Some(PlatformNodeId::from([0x78; 20]));
    p.platform_p2p_port = Some(mainnet().default_platform_p2p_port);
    p.platform_http_port = Some(mainnet().default_platform_http_port);
    (fx, p)
}

#[test]
fn service_updates_are_valid() {
    let (fx, p) = serv();
    assert_eq!(check_serv(&fx, p), Ok(()));
    let (fx, p) = serv_evo();
    assert_eq!(check_serv(&fx, p), Ok(()));
}

pub(super) fn serv_hash() -> Verdict {
    let (fx, mut p) = serv();
    p.pro_tx_hash = TxHash::from([0xee; 32]);
    check_serv(&fx, p)
}

/// A regular payload for an Evo: kills M-2t (the type comparison dropped).
pub(super) fn serv_type_mismatch() -> Verdict {
    let (fx, p) = serv();
    check_serv(&replaced(fx, updated_evo(|_, _| {})), p)
}

pub(super) fn serv_version_downgrade() -> Verdict {
    let (fx, mut p) = serv();
    let legacy = pro_up_serv(BlsScheme::Legacy);
    assert_eq!(legacy.version, ProTxVersion::LegacyBls);
    p.version = ProTxVersion::LegacyBls;
    p.sig = legacy.sig;
    check_serv(&fx, p)
}

/// Kills M-2d (V24's Evo version rule dropped).
pub(super) fn serv_version_disallowed() -> Verdict {
    let (mut fx, p) = serv_evo();
    fx.rules.v24 = true;
    check_serv(&fx, p)
}

/// A version-3 state, a version-2 payload whose address (testnet, port 22) ExtNetInfo refuses:
/// raised to version 3, it cannot carry over (§3.16.1). Kills M-2m (the migration step dropped)
/// and M-2v (the payload's version as the target).
pub(super) fn serv_migration() -> Verdict {
    let (mut fx, mut p) = serv();
    fx.params = testnet();
    let fx = replaced(
        fx,
        updated(|_, c| {
            c.version = 3;
            c.net_info = Arc::new(ext(&[(0, vec![ipv4_entry([5, 5, 5, 5], 19_999)])]));
        }),
    );
    p.net_info = mn_ipv4([1, 2, 3, 4], 22);
    check_serv(&fx, p)
}

/// The control of [`serv_migration`]: at version 2 the state does not migrate.
#[test]
fn a_service_update_migrates_only_above_its_version() {
    let (mut fx, mut p) = serv();
    fx.params = testnet();
    p.net_info = mn_ipv4([1, 2, 3, 4], 22);
    assert_eq!(check_serv(&fx, p), Ok(()));
}

/// A legacy state raised to version 2 while [`OTHER`] holds its operator key's point under the
/// basic encoding: Core's re-key would collide. Kills M-2k (the probe dropped).
pub(super) fn serv_dup_key_migration() -> Verdict {
    dup_key_migration(true)
}

/// [`serv_dup_key_migration`]'s lists and payload, with V24 active or not.
fn dup_key_migration(v24: bool) -> Verdict {
    let (mut fx, p) = serv();
    fx.rules.v24 = v24;
    let key = operator_key(UPDATED, BlsSchemeKind::Basic);
    let fx = replaced(fx, legacy_holder(UPDATED, &key)).with_mn(mn(OTHER, |_, c| {
        c.pub_key_operator = key;
    }));
    check_serv(&fx, p)
}

/// Before V24 the probe does not run (`specialtxman.cpp` rc.4:1434-1437), and
/// `ApplyProUpServTx` keeps the state's version (rc.4:113-114, 136): the same update is accepted
/// beside the cross-scheme holder. Kills M-2w (the probe's V24 gate dropped).
#[test]
fn the_dup_key_probe_waits_for_v24() {
    assert_eq!(dup_key_migration(false), Ok(()));
}

/// The probe reads a migration only: a version-2 state keeps its key, so a holder of its point
/// under the other scheme does not matter.
#[test]
fn a_service_update_without_migration_skips_the_probe() {
    let (fx, p) = serv();
    let key = operator_key(UPDATED, BlsSchemeKind::Basic);
    let fx = fx.with_mn(legacy_holder(OTHER, &key));
    assert_eq!(check_serv(&fx, p), Ok(()));
}

/// [`OTHER`] holds the payload's address. Kills M-2e (the entry loop dropped).
pub(super) fn serv_dup_netinfo_entry() -> Verdict {
    let (fx, p) = serv();
    let net_info = Arc::new(p.net_info.clone());
    let fx = fx.with_mn(mn(OTHER, |_, c| c.net_info = net_info));
    check_serv(&fx, p)
}

/// The masternode's own address may stay: kills M-2s (the holder's hash not compared).
#[test]
fn a_service_update_keeps_its_own_address() {
    let (fx, p) = serv();
    let net_info = Arc::new(p.net_info.clone());
    let fx = replaced(fx, updated(|_, c| c.net_info = net_info));
    assert_eq!(check_serv(&fx, p), Ok(()));
}

/// [`OTHER`], an Evo, holds the payload's platform node id. Kills M-2p.
pub(super) fn serv_dup_platform_node_id() -> Verdict {
    let (fx, p) = serv_evo();
    let id = p.platform_node_id.expect("an id");
    let fx = fx.with_mn(dmn_with(OTHER, MnType::Evo, |_, c| c.platform_node_id = id));
    check_serv(&fx, p)
}

/// The masternode's own platform node id may stay.
#[test]
fn a_service_update_keeps_its_own_platform_node_id() {
    let (fx, p) = serv_evo();
    let id = p.platform_node_id.expect("an id");
    let fx = replaced(fx, updated_evo(|_, c| c.platform_node_id = id));
    assert_eq!(check_serv(&fx, p), Ok(()));
}

/// An operator payee with no operator reward. Kills M-2r.
pub(super) fn serv_operator_payee_reward() -> Verdict {
    let (fx, mut p) = serv();
    p.script_operator_payout = p2pkh([0x41; 20]).into();
    check_serv(&fx, p)
}

/// An operator payee that is neither P2PKH nor P2SH. Kills M-2o.
pub(super) fn serv_operator_payee_script() -> Verdict {
    let (fx, mut p) = serv();
    let fx = replaced(fx, updated(|d, _| d.operator_reward = 100));
    p.script_operator_payout = vec![0x51].into();
    check_serv(&fx, p)
}

#[test]
fn an_operator_payee_needs_a_reward() {
    let (fx, mut p) = serv();
    let fx = replaced(fx, updated(|d, _| d.operator_reward = 100));
    p.script_operator_payout = p2pkh([0x41; 20]).into();
    assert_eq!(check_serv(&fx, p), Ok(()));
}

pub(super) fn serv_inputs_hash() -> Verdict {
    let (fx, mut p) = serv();
    p.inputs_hash = Default::default();
    verdict(fx.check(&special(TxType::ProviderUpdateService, update_inputs(), vec![], &p)))
}

/// [`serv`] with signatures checked, signed by `sign` over its hash.
fn signed_serv(sign: impl FnOnce(&[u8; 32]) -> BlsEagerSig) -> (Fixture, Tx) {
    let (mut fx, mut p) = serv();
    fx.check_sigs = true;
    p.inputs_hash = carried_inputs_hash(&update_inputs());
    p.sig = sign(&payload_sig_hash(&p).expect("a preimage"));
    (fx, special(TxType::ProviderUpdateService, update_inputs(), vec![], &p))
}

/// Signed by another operator. Kills M-4s (the signature not checked).
pub(super) fn serv_sig() -> Verdict {
    let (fx, tx) = signed_serv(|h| basic_sig(OTHER, h));
    verdict(fx.check(&tx))
}

#[test]
fn the_operator_signs_the_service_update() {
    let (fx, tx) = signed_serv(|h| basic_sig(UPDATED, h));
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
}

/// `check_sigs` off reads no signature, and the verifier is not called.
#[test]
fn unchecked_signatures_are_not_verified() {
    let (mut fx, tx) = signed_serv(|h| basic_sig(OTHER, h));
    fx.check_sigs = false;
    fx.verifier = Box::new(FixedVerifier::default());
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
}

/// A verifier without a verdict is a fault, never a verdict (D2). Kills M-4f (no verdict read as
/// valid).
#[test]
fn a_verifier_without_a_verdict_faults() {
    let (mut fx, tx) = signed_serv(|h| basic_sig(UPDATED, h));
    fx.verifier = Box::new(FixedVerifier::default());
    assert_eq!(fx.check(&tx), Err(SpecialTxFault::VerifyUnavailable));
}

/// Register quirk 21 (EV1-4): the verify scheme is the global flag, legacy before V19
/// (`bls.cpp:296-312`), not the payload's or the key's: a version-1 update of a legacy
/// masternode, signed under the legacy scheme, verifies before V19 and not after. Kills M-4v (the
/// verify scheme fixed at basic). Core confirms it on the oracle, for service updates and
/// revocations signed under either scheme on both sides of V19
/// (`node-l1/tests/l1_specialtx.rs::operator_sig_scheme_follows_the_flag_quirk_21`).
#[test]
fn quirk_21_protx_sig_scheme_follows_height() {
    let run = |v19: bool, sign: &dyn Fn(&[u8; 32]) -> BlsEagerSig| {
        let mut fx = Fixture::v24();
        fx.rules.v24 = false;
        fx.v19 = v19;
        fx.check_sigs = true;
        let fx = fx.with_mn(updated(|_, c| {
            c.version = 1;
            c.pub_key_operator = legacy_key(UPDATED);
        }));
        let mut p = pro_up_serv(BlsScheme::Legacy);
        p.pro_tx_hash = TxHash::from(*pro_tx(UPDATED).as_bytes());
        p.script_operator_payout = Vec::new().into();
        p.inputs_hash = carried_inputs_hash(&update_inputs());
        p.sig = sign(&payload_sig_hash(&p).expect("a preimage"));
        verdict(fx.check(&special(TxType::ProviderUpdateService, update_inputs(), vec![], &p)))
    };
    let legacy = |h: &[u8; 32]| legacy_sig(UPDATED, h);
    assert_eq!(run(false, &legacy), Ok(()));
    assert_reject(run(true, &legacy), C, BAD_PROTX_SIG);
}

// --- ProUpRevTx (§3.6) ---

/// A revocation of [`UPDATED`] from Core's trivially valid basic payload.
fn rev() -> (Fixture, ProUpRevTx) {
    let fx = Fixture::v24().with_mn(updated(|_, _| {}));
    let mut p = pro_up_rev(BlsScheme::Basic);
    p.pro_tx_hash = TxHash::from(*pro_tx(UPDATED).as_bytes());
    (fx, p)
}

fn check_rev(fx: &Fixture, mut p: ProUpRevTx) -> Verdict {
    let inputs = update_inputs();
    p.inputs_hash = carried_inputs_hash(&inputs);
    verdict(fx.check(&special(TxType::ProviderUpdateRevoke, inputs, vec![], &p)))
}

#[test]
fn revocation_is_valid() {
    let (fx, p) = rev();
    assert_eq!(check_rev(&fx, p), Ok(()));
}

pub(super) fn rev_hash() -> Verdict {
    let (fx, mut p) = rev();
    p.pro_tx_hash = TxHash::from([0xee; 32]);
    check_rev(&fx, p)
}

/// Kills M-4d (the version change not checked).
pub(super) fn rev_version_downgrade() -> Verdict {
    let (fx, mut p) = rev();
    let legacy = pro_up_rev(BlsScheme::Legacy);
    assert_eq!(legacy.version, ProTxVersion::LegacyBls);
    p.version = ProTxVersion::LegacyBls;
    p.sig = legacy.sig;
    check_rev(&fx, p)
}

/// Kills M-4i (the inputs hash not checked).
pub(super) fn rev_inputs_hash() -> Verdict {
    let (fx, mut p) = rev();
    p.inputs_hash = Default::default();
    verdict(fx.check(&special(TxType::ProviderUpdateRevoke, update_inputs(), vec![], &p)))
}

/// [`UPDATED`] revoked at state `version` (`ResetOperatorFields`, `dmnstate.h` rc.4:127-135; a
/// revocation before V24 leaves version 1, one after it keeps the state's): a null operator key.
fn revoked(version: i32) -> Arc<Dmn> {
    updated(|_, c| {
        c.reset_operator_fields(BlsSchemeKind::Legacy);
        c.version = version;
    })
}

/// An update of a revoked masternode: with signatures checked, no signature verifies against
/// the null operator key (`bls.cpp` rc.4:298-300), so it is `bad-protx-sig`; unchecked, the
/// update passes, a version-1 state's migration probe included (a null key is no holder's).
#[test]
fn updates_of_a_revoked_masternode() {
    for version in [1, 2] {
        for check_sigs in [false, true] {
            let (fx, mut p) = serv();
            let mut fx = replaced(fx, revoked(version));
            fx.check_sigs = check_sigs;
            p.inputs_hash = carried_inputs_hash(&update_inputs());
            p.sig = basic_sig(UPDATED, &payload_sig_hash(&p).expect("a preimage"));
            let tx = special(TxType::ProviderUpdateService, update_inputs(), vec![], &p);
            let serv = verdict(fx.check(&tx));
            let (fx, mut p) = rev();
            let mut fx = replaced(fx, revoked(version));
            fx.check_sigs = check_sigs;
            p.inputs_hash = carried_inputs_hash(&update_inputs());
            p.sig = basic_sig(UPDATED, &payload_sig_hash(&p).expect("a preimage"));
            let tx = special(TxType::ProviderUpdateRevoke, update_inputs(), vec![], &p);
            let rev = verdict(fx.check(&tx));
            for (name, v) in [("service update", serv), ("revocation", rev)] {
                if check_sigs {
                    assert_reject(v, C, BAD_PROTX_SIG);
                } else {
                    assert_eq!(v, Ok(()), "{name} of a revoked version-{version} state");
                }
            }
        }
    }
}

/// [`rev`] with signatures checked, signed by operator `n`.
fn signed_rev(n: u8) -> (Fixture, Tx) {
    let (mut fx, mut p) = rev();
    fx.check_sigs = true;
    p.inputs_hash = carried_inputs_hash(&update_inputs());
    p.sig = basic_sig(n, &payload_sig_hash(&p).expect("a preimage"));
    (fx, special(TxType::ProviderUpdateRevoke, update_inputs(), vec![], &p))
}

/// Kills M-4r (the revocation's signature not checked).
pub(super) fn rev_sig() -> Verdict {
    let (fx, tx) = signed_rev(OTHER);
    verdict(fx.check(&tx))
}

#[test]
fn the_operator_signs_the_revocation() {
    let (fx, tx) = signed_rev(UPDATED);
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
}
