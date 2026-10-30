//! L0 vectors of EV1-3's and EV1-4's checks (EV1 §6.1): the dispatcher, types 1 to 4 ([`updates`]
//! for 2 and 4) and 10 to 12, the
//! shared-collateral covenant, the D13/D26 coins-view pair, the D17 operator-key equality, the
//! L0 half of register quirk 26, and census closure over every reject site EV1-3 lands.
//!
//! Each reject vector is a [`CASES`] row: its Core site, its expected class and reason, and a
//! function building the input and returning our verdict. The expected values are Core's, read
//! from the cited lines at rc.4 (`specialtxman.cpp`) or ported from Core's unit tests
//! (`evo_sharedmn_tests.cpp`).

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use dash_consensus::primitives::Tx;
use dash_consensus::subsidy::COIN;
use dash_consensus::validation::{TxValidationError, TxValidationResult};
use dash_core_vectors::cases::BlsScheme;
use dash_evo::spork::message_hash;
use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind, BlsSchemeSource, BlsValue};
use dash_primitives::{
    CollateralShare, MnType as PayloadMnType, PayoutShare, PlatformNodeId, ProDisTx, ProRegTx,
    ProUpRegTx, ProUpShareTx, ProUpSharedRegTx, Transaction, TxHash, TxIn, TxOut, TxType,
};
use secp256k1::SecretKey;

use super::covenant::{
    SHARED_COLLATERAL_SCRIPT, is_shared_collateral_script, shared_collateral_spends,
    shared_collateral_template_outputs,
};
use super::network::tests::ext;
use super::reasons::*;
use super::shared::PRODIS_MAX_FEE;
use super::sig::{payload_sig_hash, pro_dis_sign_hash, shared_reg_consent_hash};
use super::testctx::*;
use super::trivial::tests::{
    ext_regular, key, p2pkh, p2sh, payout, pro_reg, pro_reg_shared, pro_reg_v3, pro_up_reg,
    pro_up_share, pro_up_shared_reg,
};
use super::*;
use crate::testkit::{dmn_with, operator_key, pro_tx};
use crate::types::{KeyId, OperatorKey};
use crate::{Dmn, MnCold, MnType};

use TxValidationResult::{BadSpecial as S, Consensus as C};

type Verdict = Result<(), TxValidationError>;

/// The verdict of a check that must not fault.
fn verdict(result: CheckResult<Infallible>) -> Verdict {
    result.expect("no fault")
}

fn is(verdict: &Verdict, class: TxValidationResult, reason: &str) -> bool {
    verdict.as_ref().is_err_and(|e| e.result() == class && e.reject_reason() == reason)
}

#[track_caller]
fn assert_reject(verdict: Verdict, class: TxValidationResult, reason: &str) {
    assert!(is(&verdict, class, reason), "{verdict:?}, expected {class:?} {reason}");
}

// --- The dispatcher (§3.2) ---

/// A transaction of `tx_type` with a payload field holding `payload`.
fn typed(tx_type: TxType, payload: Vec<u8>) -> Tx {
    tx_of(tx_type, vec![txin([1; 32], 0)], vec![txout(COIN, p2pkh([1; 20]))], payload)
}

/// DEC-663 A1: a type whose checks have not landed faults past DIP3's gate, never accepting and
/// never rejecting; before it, the gate rejects every type first.
#[test]
fn every_unported_type_faults() {
    let unported = [
        TxType::CoinbaseCommitment,
        TxType::QuorumCommitment,
        TxType::MnhfSignal,
        TxType::AssetLock,
        TxType::AssetUnlock,
    ];
    for t in unported {
        let mut fx = Fixture::v24();
        for payload in [vec![], vec![1, 0], vec![0xff; 300]] {
            assert_eq!(fx.check(&typed(t, payload)), Err(SpecialTxFault::Unported(t)), "{t:?}");
        }
        fx.dip3 = false;
        let before_dip3 = verdict(fx.check(&typed(t, vec![1, 0])));
        assert_reject(before_dip3, S, BAD_TX_TYPE_DIP3_INACTIVE);
    }
}

/// Every type the dispatcher sends to a check, with a payload nothing decodes: the check's own
/// `bad-protx-payload`, not a fault.
#[test]
fn ported_types_reach_their_checks() {
    for t in [
        TxType::ProviderRegister,
        TxType::ProviderUpdateService,
        TxType::ProviderUpdateRegistrar,
        TxType::ProviderUpdateRevoke,
        TxType::ProviderDissolve,
        TxType::ProviderUpdateShare,
        TxType::ProviderUpdateSharedRegistrar,
    ] {
        assert_reject(verdict(Fixture::v24().check(&typed(t, vec![0xff]))), C, BAD_PROTX_PAYLOAD);
    }
}

fn dispatch_no_payload_field() -> Verdict {
    // A version-2 transaction carries no payload field, whatever its type bits.
    let mut fx = Fixture::v24();
    fx.dip3 = false;
    let raw = Transaction {
        version: 2,
        tx_type: TxType::ProviderRegister,
        inputs: vec![txin([1; 32], 0)],
        outputs: vec![txout(COIN, p2pkh([1; 20]))],
        lock_time: 0,
        extra_payload: vec![],
    };
    verdict(fx.check(&Tx::new(raw)))
}

fn dispatch_dip3_inactive() -> Verdict {
    let mut fx = Fixture::v24();
    fx.dip3 = false;
    verdict(fx.check(&typed(TxType::ProviderRegister, vec![])))
}

fn dispatch_unknown_type() -> Verdict {
    verdict(Fixture::v24().check(&typed(TxType::Unknown(99), vec![1])))
}

/// A coins view that fails a read faults the check, at either collateral read: no verdict.
#[test]
fn coins_faults_propagate() {
    let (fx, mut p) = ext_reg();
    let reg = reg_tx(&mut p, vec![]);
    let (up_fx, mut up) = up_reg();
    let up = up_reg_tx(&mut up);
    for (fx, tx) in [(&fx, &reg), (&up_fx, &up)] {
        let fault = fx.with_view(&FailingCoins, |cx| check_special_tx_inner(tx, tx.payload(), cx));
        assert_eq!(fault, Err(SpecialTxFault::Coins("read failure")), "{:?}", tx.tx_type());
    }
}

// --- ProRegTx (§3.3) ---

const EXT_COLLATERAL: [u8; 32] = [7; 32];

/// The registrations' collateral key.
fn collateral_signer() -> (SecretKey, KeyId) {
    signer(0x51)
}

fn collateral_script() -> Vec<u8> {
    p2pkh(*collateral_signer().1.as_bytes())
}

/// Sets the payload's `inputsHash` for `inputs` and builds the registration.
fn reg_tx_with(p: &mut ProRegTx, inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Tx {
    p.inputs_hash = carried_inputs_hash(&inputs);
    special(TxType::ProviderRegister, inputs, outputs, p)
}

fn reg_tx(p: &mut ProRegTx, outputs: Vec<TxOut>) -> Tx {
    reg_tx_with(p, vec![txin([9; 32], 0)], outputs)
}

/// A valid version-3 Regular registration with an external collateral, the collateral in the
/// view.
fn ext_reg() -> (Fixture, ProRegTx) {
    let mut p = pro_reg_v3();
    p.collateral_hash = TxHash::from(EXT_COLLATERAL);
    p.collateral_index = 1;
    p.vch_sig.clear();
    let mut fx = Fixture::v24();
    fx.coins = ModelCoins::default()
        .with(outpoint(EXT_COLLATERAL, 1), txout(1000 * COIN, collateral_script()));
    (fx, p)
}

/// `p` checked in a registration whose one output is not a collateral.
fn check_ext(fx: &Fixture, mut p: ProRegTx) -> Verdict {
    verdict(fx.check(&reg_tx(&mut p, vec![txout(COIN, p2pkh([0x55; 20]))])))
}

/// A valid version-3 Regular registration with its collateral as output 0.
fn int_reg() -> ProRegTx {
    let mut p = pro_reg_v3();
    p.collateral_hash = TxHash::default();
    p.collateral_index = 0;
    p.vch_sig.clear();
    p
}

fn check_int(fx: &Fixture, mut p: ProRegTx, collateral: TxOut) -> Verdict {
    verdict(fx.check(&reg_tx(&mut p, vec![collateral, txout(COIN, p2pkh([0x55; 20]))])))
}

fn int_collateral() -> TxOut {
    txout(1000 * COIN, collateral_script())
}

/// A valid shared registration of two shares, its collateral the template output 0.
fn shared_reg() -> ProRegTx {
    let mut p = pro_reg_shared();
    p.collateral_index = 0;
    p.vch_sig.clear();
    p.join_sigs = vec![compact(&[0; 65]); p.shares.len()];
    p
}

fn template_collateral(value: i64) -> TxOut {
    txout(value, SHARED_COLLATERAL_SCRIPT.to_vec())
}

/// An Evo registration: version 3, an empty ExtNetInfo (`allow_empty`), a platformNodeID; its
/// external collateral of 4000 DASH in the view.
fn evo_reg() -> (Fixture, ProRegTx) {
    let (mut fx, mut p) = ext_reg();
    p.mn_type = PayloadMnType::Evo;
    p.net_info = ext(&[]);
    p.platform_node_id = Some(PlatformNodeId::from([0x77; 20]));
    fx.coins = ModelCoins::default()
        .with(outpoint(EXT_COLLATERAL, 1), txout(4000 * COIN, collateral_script()));
    (fx, p)
}

/// Masternode `n` with operator key `n` (basic), version 2, edited by `f`.
fn mn(n: u8, f: impl FnOnce(&mut Dmn, &mut MnCold)) -> Arc<Dmn> {
    dmn_with(n, MnType::Regular, |d, c| {
        c.version = 2;
        c.pub_key_operator = operator_key(n, BlsSchemeKind::Basic);
        f(d, c)
    })
}

#[test]
fn registrations_are_valid() {
    let (fx, p) = ext_reg();
    assert_eq!(check_ext(&fx, p), Ok(()));
    // Version 2 (basic) is the lowest V24 allows.
    let mut basic = pro_reg(BlsScheme::Basic);
    basic.collateral_hash = TxHash::default();
    basic.collateral_index = 0;
    basic.vch_sig.clear();
    assert_eq!(check_int(&Fixture::v24(), basic, int_collateral()), Ok(()));
    // One payout, the most an Evo has without EVO_SHARES.
    let (fx, mut p) = evo_reg();
    p.payouts = vec![payout(0x31, 10_000)];
    assert_eq!(check_ext(&fx, p), Ok(()));
    assert_eq!(check_int(&Fixture::v24(), int_reg(), int_collateral()), Ok(()));
    let p = shared_reg();
    assert_eq!(check_int(&Fixture::v24(), p, template_collateral(1000 * COIN)), Ok(()));
    let (mut fx, p) = evo_reg();
    fx.rules.evo_shares = true;
    assert_eq!(check_ext(&fx, p), Ok(()));
}

fn reg_version_disallowed() -> Verdict {
    // V24 refuses a legacy-scheme registration, version 1.
    let mut p = pro_reg(BlsScheme::Legacy);
    assert_eq!(p.version, 1);
    p.vch_sig.clear();
    let mut fx = Fixture::v24();
    fx.coins = ModelCoins::default()
        .with(outpoint(*p.collateral_hash.as_bytes(), p.collateral_index), int_collateral());
    verdict(fx.check(&reg_tx(&mut p, vec![int_collateral()])))
}

fn reg_shares_evo() -> Verdict {
    let mut p = shared_reg();
    p.mn_type = PayloadMnType::Evo;
    p.net_info = ext(&[]);
    p.platform_node_id = Some(PlatformNodeId::from([0x77; 20]));
    p.shares = vec![shares_of(1, 2400 * COIN), shares_of(2, 1600 * COIN)];
    check_int(&Fixture::v24(), p, template_collateral(4000 * COIN))
}

fn shares_of(n: u8, amount: i64) -> CollateralShare {
    let mut s = super::trivial::tests::share(n, amount);
    s.key_id_owner = key(0x20 + n);
    s
}

fn reg_payouts_evo() -> Verdict {
    let (fx, p) = evo_reg();
    assert_eq!(p.payouts.len(), 2);
    check_ext(&fx, p)
}

fn reg_collateral_missing() -> Verdict {
    let (mut fx, p) = ext_reg();
    fx.coins = ModelCoins::default();
    check_ext(&fx, p)
}

fn reg_collateral_value() -> Verdict {
    let (mut fx, p) = ext_reg();
    fx.coins = ModelCoins::default()
        .with(outpoint(EXT_COLLATERAL, 1), txout(999 * COIN, collateral_script()));
    check_ext(&fx, p)
}

fn reg_collateral_dest() -> Verdict {
    let (mut fx, p) = ext_reg();
    fx.coins =
        ModelCoins::default().with(outpoint(EXT_COLLATERAL, 1), txout(1000 * COIN, vec![0x51]));
    check_ext(&fx, p)
}

fn reg_collateral_pkh() -> Verdict {
    let (mut fx, p) = ext_reg();
    fx.coins = ModelCoins::default()
        .with(outpoint(EXT_COLLATERAL, 1), txout(1000 * COIN, p2sh([0x52; 20])));
    check_ext(&fx, p)
}

fn reg_collateral_index() -> Verdict {
    let mut p = int_reg();
    p.collateral_index = 2;
    check_int(&Fixture::v24(), p, int_collateral())
}

fn reg_internal_collateral_value() -> Verdict {
    check_int(&Fixture::v24(), int_reg(), txout(1000 * COIN - 1, collateral_script()))
}

fn reg_shares_collateral_script() -> Verdict {
    check_int(&Fixture::v24(), shared_reg(), int_collateral())
}

fn reg_internal_collateral_dest() -> Verdict {
    check_int(&Fixture::v24(), int_reg(), txout(1000 * COIN, vec![0x51]))
}

fn reg_collateral_reuse() -> Verdict {
    // The collateral pays the owner key.
    let p = int_reg();
    let owner = *p.key_id_owner.as_bytes();
    check_int(&Fixture::v24(), p, txout(1000 * COIN, p2pkh(owner)))
}

fn reg_payee_reuse() -> Verdict {
    // A payout pays the collateral (version 3 checks the payouts).
    let mut p = int_reg();
    p.payouts[0].script_payout = collateral_script().into();
    check_int(&Fixture::v24(), p, int_collateral())
}

/// Version 2 does not check the payouts against the collateral (`ExtAddr` and up do).
#[test]
fn payout_collateral_reuse_from_ext_addr() {
    let mut p = pro_reg(BlsScheme::Basic);
    p.collateral_hash = TxHash::default();
    p.collateral_index = 0;
    p.vch_sig.clear();
    p.script_payout = collateral_script().into();
    let mut fx = Fixture::v24();
    fx.rules.v24 = false;
    assert_eq!(check_int(&fx, p, int_collateral()), Ok(()));
}

fn reg_dup_netinfo_entry() -> Verdict {
    let (fx, p) = ext_reg();
    let fx = fx.with_mn(mn(1, |_, c| {
        c.version = 3;
        c.net_info = Arc::new(ext_regular());
    }));
    check_ext(&fx, p)
}

/// An entry held by the masternode on the same collateral, which the registration replaces.
#[test]
fn same_collateral_may_hold_the_entries() {
    let (fx, p) = ext_reg();
    let fx = fx.with_mn(mn(1, |d, c| {
        d.collateral = outpoint(EXT_COLLATERAL, 1);
        c.version = 3;
        c.net_info = Arc::new(ext_regular());
    }));
    assert_eq!(check_ext(&fx, p), Ok(()));
}

fn reg_dup_owner_key() -> Verdict {
    let (fx, p) = ext_reg();
    let owner = p.key_id_owner;
    check_ext(&fx.with_mn(mn(1, |_, c| c.key_id_owner = owner)), p)
}

fn reg_dup_operator_key() -> Verdict {
    let (fx, p) = ext_reg();
    let operator = p.pub_key_operator.clone();
    check_ext(&fx.with_mn(mn(1, |_, c| c.pub_key_operator = operator)), p)
}

fn reg_dup_share_owner_key() -> Verdict {
    let p = shared_reg();
    let owner = p.shares[1].key_id_owner;
    let fx = Fixture::v24().with_mn(mn(1, |_, c| c.key_id_owner = owner));
    check_int(&fx, p, template_collateral(1000 * COIN))
}

fn reg_shared_dup_operator_key() -> Verdict {
    let p = shared_reg();
    let operator = p.pub_key_operator.clone();
    let fx = Fixture::v24().with_mn(mn(1, |_, c| c.pub_key_operator = operator));
    check_int(&fx, p, template_collateral(1000 * COIN))
}

/// A masternode holding `key`'s point under the legacy scheme (a version-1 state).
fn legacy_holder(n: u8, key: &BlsLazyPk) -> Arc<Dmn> {
    let point = key.get();
    mn(n, |_, c| {
        c.version = 1;
        c.pub_key_operator = BlsLazyPk::from_value(point, BlsSchemeKind::Legacy);
    })
}

fn reg_dup_key_any_scheme() -> Verdict {
    let (fx, p) = ext_reg();
    let fx = fx.with_mn(legacy_holder(1, &p.pub_key_operator));
    check_ext(&fx, p)
}

/// Before V24, `GetValidatedPayload`'s ceiling is V19's: the basic scheme (version 2) from V19
/// on, the legacy one before (`providertx.h` rc.4 `GetMaxFromDeployment`).
#[test]
fn v19_admits_the_basic_scheme() {
    for v19 in [true, false] {
        let mut p = pro_reg(BlsScheme::Basic);
        p.collateral_hash = TxHash::default();
        p.collateral_index = 0;
        p.vch_sig.clear();
        let mut fx = Fixture::v24();
        fx.rules.v24 = false;
        fx.v19 = v19;
        let got = check_int(&fx, p, int_collateral());
        assert_eq!(is(&got, C, BAD_PROTX_VERSION), !v19, "v19 {v19}: {got:?}");
    }
}

/// The cross-scheme probe is V24's (`specialtxman.cpp` rc.4:1333).
#[test]
fn any_scheme_probe_is_v24_only() {
    let mut p = pro_reg(BlsScheme::Basic);
    p.collateral_hash = TxHash::default();
    p.collateral_index = 0;
    p.vch_sig.clear();
    let mut fx = Fixture::v24().with_mn(legacy_holder(1, &p.pub_key_operator));
    fx.rules.v24 = false;
    assert_eq!(check_int(&fx, p, int_collateral()), Ok(()));
}

fn reg_dup_platform_node_id() -> Verdict {
    let (mut fx, mut p) = evo_reg();
    fx.rules.evo_shares = true;
    p.payouts.truncate(1);
    p.payouts[0].reward = 10_000;
    let fx = fx.with_mn(dmn_with(1, MnType::Evo, |_, c| {
        c.version = 2;
        c.pub_key_operator = operator_key(1, BlsSchemeKind::Basic);
        c.platform_node_id = PlatformNodeId::from([0x77; 20]);
    }));
    check_ext(&fx, p)
}

fn reg_key_not_same() -> Verdict {
    let (mut fx, mut p) = ext_reg();
    fx.height = NOT_ENFORCED;
    p.key_id_voting = key(0x3c);
    assert_ne!(p.key_id_owner, p.key_id_voting);
    check_ext(&fx, p)
}

/// Before DIP3's enforcement the owner and voting keys must be one key.
#[test]
fn same_keys_before_enforcement() {
    let (mut fx, mut p) = ext_reg();
    fx.height = NOT_ENFORCED;
    p.key_id_voting = p.key_id_owner;
    assert_eq!(check_ext(&fx, p), Ok(()));
}

/// Enforcement is read at `pindexPrev` (`DeploymentDIP0003Enforced(pindexPrev->nHeight)`,
/// `specialtxman.cpp` rc.4:1343, 1572): the block at mainnet's enforcement height still requires
/// one key, the next does not.
#[test]
fn key_not_same_reads_enforcement_at_pindex_prev() {
    let enforcement = mainnet_dip3_enforcement();
    for (height, expect_same) in [(enforcement, true), (enforcement + 1, false)] {
        let (mut fx, mut p) = ext_reg();
        fx.height = height;
        p.key_id_voting = key(0x3c);
        let got = check_ext(&fx, p.clone());
        assert_eq!(is(&got, S, BAD_PROTX_KEY_NOT_SAME), expect_same, "{height}: {got:?}");
        let (mut fx, mut p) = up_reg();
        fx.height = height;
        p.key_id_voting = key(0x3c);
        let got = check_up(&fx, p);
        assert_eq!(is(&got, S, BAD_PROTX_KEY_NOT_SAME), expect_same, "{height}: {got:?}");
    }
}

fn mainnet_dip3_enforcement() -> u32 {
    Fixture::v24().params().consensus.dip0003_enforcement.0.to_u32()
}

fn reg_inputs_hash() -> Verdict {
    let (fx, mut p) = ext_reg();
    let mut tx_inputs = vec![txin([9; 32], 0)];
    p.inputs_hash = carried_inputs_hash(&tx_inputs);
    tx_inputs.push(txin([9; 32], 1));
    verdict(fx.check(&special(TxType::ProviderRegister, tx_inputs, vec![], &p)))
}

fn reg_internal_sig() -> Verdict {
    let mut p = int_reg();
    p.vch_sig = vec![1; 65];
    check_int(&Fixture::v24(), p, int_collateral())
}

/// [`ext_reg`] signed by `key` over its sign string.
fn signed_ext(key: &SecretKey) -> (Fixture, Tx) {
    let (mut fx, mut p) = ext_reg();
    fx.check_sigs = true;
    let inputs = vec![txin([9; 32], 0)];
    p.inputs_hash = carried_inputs_hash(&inputs);
    let message = super::sig::make_sign_string(&p, fx.params()).expect("a sign string");
    p.vch_sig = sign(&message_hash(&message), key);
    let tx = special(TxType::ProviderRegister, inputs, vec![], &p);
    (fx, tx)
}

fn reg_external_sig() -> Verdict {
    let (fx, tx) = signed_ext(&signer(0x99).0);
    verdict(fx.check(&tx))
}

#[test]
fn external_collateral_key_signs() {
    let (fx, tx) = signed_ext(&collateral_signer().0);
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
}

/// [`shared_reg`] with two real share owners, each signing the consent hash; `bad` signs share 1's
/// with another key.
fn signed_shared(bad: bool) -> (Fixture, Tx) {
    let mut fx = Fixture::v24();
    fx.check_sigs = true;
    let mut p = shared_reg();
    let owners = [signer(0x61), signer(0x62)];
    for (share, (_, id)) in p.shares.iter_mut().zip(&owners) {
        share.key_id_owner = *id;
    }
    let outputs = vec![template_collateral(1000 * COIN)];
    let tx = reg_tx(&mut p, outputs.clone());
    let consent = shared_reg_consent_hash(&p, &tx).expect("a consent hash");
    p.join_sigs = owners.iter().map(|(k, _)| compact(&sign(&consent, k))).collect();
    if bad {
        p.join_sigs[1] = compact(&sign(&consent, &signer(0x63).0));
    }
    let tx = reg_tx(&mut p, outputs);
    // The join signatures are not part of what they sign.
    assert_eq!(shared_reg_consent_hash(&p, &tx), Some(consent));
    (fx, tx)
}

fn reg_shares_sig() -> Verdict {
    let (fx, tx) = signed_shared(true);
    verdict(fx.check(&tx))
}

#[test]
fn share_owners_consent() {
    let (fx, tx) = signed_shared(false);
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
}

// --- ProUpRegTx (§3.5) ---

const UP_COLLATERAL: [u8; 32] = [0x33; 32];

fn owner_signer() -> (SecretKey, KeyId) {
    signer(0x71)
}

/// A registrar update of masternode 1 (version 2, basic, its own owner key) to Core's trivially
/// valid basic payload, which changes the operator key; the collateral in the view.
fn up_reg() -> (Fixture, ProUpRegTx) {
    let mut fx = Fixture::v24();
    fx.coins = ModelCoins::default()
        .with(outpoint(UP_COLLATERAL, 0), txout(1000 * COIN, collateral_script()));
    let fx = fx.with_mn(up_reg_mn(|_, _| {}));
    let mut p = pro_up_reg(BlsScheme::Basic);
    p.pro_tx_hash = TxHash::from(*pro_tx(1).as_bytes());
    p.vch_sig.clear();
    (fx, p)
}

fn up_reg_mn(f: impl FnOnce(&mut Dmn, &mut MnCold)) -> Arc<Dmn> {
    mn(1, |d, c| {
        d.collateral = outpoint(UP_COLLATERAL, 0);
        c.key_id_owner = owner_signer().1;
        f(d, c)
    })
}

fn up_reg_tx(p: &mut ProUpRegTx) -> Tx {
    let inputs = vec![txin([0x34; 32], 0)];
    p.inputs_hash = carried_inputs_hash(&inputs);
    special(TxType::ProviderUpdateRegistrar, inputs, vec![], p)
}

fn check_up(fx: &Fixture, mut p: ProUpRegTx) -> Verdict {
    verdict(fx.check(&up_reg_tx(&mut p)))
}

/// `fx` with masternode 1 replaced by `dmn`.
fn replaced(fx: Fixture, dmn: Arc<Dmn>) -> Fixture {
    let mut fx = fx;
    fx.list = MnList::scratch();
    fx.with_mn(dmn)
}

#[test]
fn registrar_update_is_valid() {
    let (fx, p) = up_reg();
    assert_eq!(check_up(&fx, p), Ok(()));
}

fn up_reg_hash() -> Verdict {
    let (fx, mut p) = up_reg();
    p.pro_tx_hash = TxHash::from([0xee; 32]);
    check_up(&fx, p)
}

fn up_reg_shared_mn() -> Verdict {
    let (fx, p) = up_reg();
    let fx = replaced(
        fx,
        up_reg_mn(|_, c| {
            c.version = 3;
            c.net_info = Arc::new(ext(&[]));
            c.shares = vec![shares_of(1, 600 * COIN), shares_of(2, 400 * COIN)];
        }),
    );
    check_up(&fx, p)
}

fn up_reg_version_downgrade() -> Verdict {
    let (fx, _) = up_reg();
    let mut p = pro_up_reg(BlsScheme::Legacy);
    assert_eq!(p.version, 1);
    p.pro_tx_hash = TxHash::from(*pro_tx(1).as_bytes());
    p.vch_sig.clear();
    check_up(&fx, p)
}

fn up_reg_dup_key_any_scheme() -> Verdict {
    let (fx, p) = up_reg();
    check_up(&fx.with_mn(legacy_holder(2, &p.pub_key_operator)), p)
}

/// The cross-scheme probe runs only for a key change or a scheme migration: an update that
/// keeps the key and the scheme passes it although another masternode holds the key under the
/// other scheme (`specialtxman.cpp` rc.4:1515-1526).
#[test]
fn unchanged_key_skips_the_any_scheme_probe() {
    let (fx, p) = up_reg();
    let operator = p.pub_key_operator.clone();
    let fx = replaced(fx, up_reg_mn(|_, c| c.pub_key_operator = operator));
    let fx = fx.with_mn(legacy_holder(2, &p.pub_key_operator));
    assert_eq!(check_up(&fx, p), Ok(()));
}

/// Masternode 1 at version 1 holding the point of `p`'s key under the legacy scheme, then `f`.
fn legacy_state(p: &ProUpRegTx, f: impl FnOnce(&mut MnCold)) -> Fixture {
    let (fx, _) = up_reg();
    let point = p.pub_key_operator.get();
    replaced(
        fx,
        up_reg_mn(|_, c| {
            c.version = 1;
            c.pub_key_operator = BlsLazyPk::from_value(point, BlsSchemeKind::Legacy);
            f(c)
        }),
    )
}

/// [`legacy_state`], with masternode 2 holding the point under the basic scheme.
fn legacy_state_update(p: &ProUpRegTx, f: impl FnOnce(&mut MnCold)) -> Fixture {
    let point = p.pub_key_operator.get();
    legacy_state(p, f).with_mn(mn(2, |_, c| {
        c.pub_key_operator = BlsLazyPk::from_value(point, BlsSchemeKind::Basic)
    }))
}

fn up_reg_dup_key_migration() -> Verdict {
    // The same point raised out of the legacy scheme while masternode 2 holds it: the migration's
    // probe (`specialtxman.cpp` rc.4:1515-1526) rejects it. Alone, the unique-key check (1566)
    // would reject it alike; the two-fault vectors below show the probe runs first.
    let (_, p) = up_reg();
    assert_eq!(p.version, 2);
    check_up(&legacy_state_update(&p, |_| {}), p)
}

fn up_reg_dup_key_before_payouts() -> Verdict {
    // The migration's probe (rc.4:1520-1527) runs before the payout list (1542): with a payout to
    // the owner key as well, Core rejects bad-protx-dup-key, not bad-protx-payee-reuse.
    let (_, mut p) = up_reg();
    let fx = legacy_state_update(&p, |_| {});
    p.script_payout = p2pkh(*owner_signer().1.as_bytes()).into();
    check_up(&fx, p)
}

/// The control of [`up_reg_dup_key_before_payouts`]: without masternode 2 the payout fault
/// rejects.
#[test]
fn a_migration_reaches_the_payout_list() {
    let (_, mut p) = up_reg();
    let fx = legacy_state(&p, |_| {});
    p.script_payout = p2pkh(*owner_signer().1.as_bytes()).into();
    assert_reject(check_up(&fx, p), S, BAD_PROTX_PAYEE_REUSE);
}

/// A version-3 update of masternode 1's point, as [`up_reg_migration`] raises it.
fn ext_up_reg() -> ProUpRegTx {
    let (_, _, mut p, _) = super::trivial::tests::ext_addr_payloads();
    p.pro_tx_hash = TxHash::from(*pro_tx(1).as_bytes());
    p.vch_sig.clear();
    p
}

/// A port ExtNetInfo refuses (22), so the migration to ExtAddr rejects.
fn bad_port(c: &mut MnCold) {
    c.net_info = Arc::new(super::network::tests::mn_ipv4([1, 2, 3, 4], 22));
}

fn up_reg_dup_key_before_migration() -> Verdict {
    // The probe (rc.4:1520-1527) runs before the migration (1529-1538): a legacy state whose
    // address ExtAddr refuses, raised to version 3 while masternode 2 holds the point, is
    // bad-protx-dup-key, not bad-protx-netinfo-version.
    let p = ext_up_reg();
    check_up(&legacy_state_update(&p, bad_port), p)
}

/// The control of [`up_reg_dup_key_before_migration`]: without masternode 2 the migration
/// rejects.
#[test]
fn a_migration_reaches_its_netinfo_step() {
    let p = ext_up_reg();
    assert_reject(check_up(&legacy_state(&p, bad_port), p), S, BAD_PROTX_NETINFO_VERSION);
}

/// A legacy state updated at version 1 keeps its scheme: no migration, no probe.
#[test]
fn a_legacy_update_is_not_a_migration() {
    let mut p = pro_up_reg(BlsScheme::Legacy);
    assert_eq!(p.version, 1);
    p.pro_tx_hash = TxHash::from(*pro_tx(1).as_bytes());
    p.vch_sig.clear();
    let fx = legacy_state_update(&p, |_| {});
    assert_eq!(check_up(&fx, p), Ok(()));
}

/// EVO_SHARES lifts the one-payout limit of an Evo; a regular masternode never had one
/// (`specialtxman.cpp` rc.4:1542-1545).
#[test]
fn evo_payout_limit_is_evo_shares_only() {
    let two = || vec![payout(0x31, 6000), payout(0x32, 4000)];
    assert_eq!(up_reg_payouts(MnType::Evo, true, two()), Ok(()));
    assert_eq!(up_reg_payouts(MnType::Evo, false, vec![payout(0x31, 10_000)]), Ok(()));
    assert_eq!(up_reg_payouts(MnType::Regular, false, two()), Ok(()));
}

fn up_reg_migration() -> Verdict {
    // The same key, raised to ExtAddr: the state's address (port 22, a bad port ExtNetInfo
    // refuses) cannot carry over.
    let (fx, _) = up_reg();
    let p = ext_up_reg();
    let operator = p.pub_key_operator.clone();
    let fx = replaced(
        fx,
        up_reg_mn(|_, c| {
            c.pub_key_operator = operator;
            bad_port(c);
        }),
    );
    check_up(&fx, p)
}

/// Masternode 1 at version 1 with a port ExtAddr refuses, its key `bytes` as a list's disk bytes
/// leave it: raw under the legacy scheme, never read.
fn raw_legacy_state(bytes: [u8; 48]) -> Fixture {
    let (fx, _) = up_reg();
    replaced(
        fx,
        up_reg_mn(|_, c| {
            c.version = 1;
            c.pub_key_operator =
                BlsLazyPk::from_bytes(&bytes, BlsSchemeSource::Version(BlsSchemeKind::Legacy));
            bad_port(c);
        }),
    )
}

/// `key_changed` at its call site (rc.4:1517, D17): Core compares the operator keys with
/// `CBLSLazyWrapper::operator==`, so the same point under the other encoding is the same key and
/// the same bytes decoding to another point are another. An unchanged key runs the migration,
/// which rejects the state's address; a changed one skips it.
#[test]
fn key_changed_compares_points_not_bytes() {
    // P under basic in the update, P's legacy bytes in the state: unchanged.
    let p = ext_up_reg();
    let legacy = BlsLazyPk::from_value(p.pub_key_operator.get(), BlsSchemeKind::Legacy).to_bytes();
    assert_ne!(legacy, p.pub_key_operator.to_bytes());
    assert_reject(check_up(&raw_legacy_state(legacy), p), S, BAD_PROTX_NETINFO_VERSION);

    // sub_100's bytes, canonical under both schemes at two points: changed.
    let bytes = corpus_input("sub_100");
    let under = |scheme| BlsLazyPk::from_bytes(&bytes, BlsSchemeSource::Version(scheme));
    let (as_legacy, as_basic) =
        (under(BlsSchemeKind::Legacy).get(), under(BlsSchemeKind::Basic).get());
    assert!(as_legacy.is_valid() && as_basic.is_valid());
    assert_ne!(as_legacy.to_bytes(BlsSchemeKind::Basic), as_basic.to_bytes(BlsSchemeKind::Basic));
    let mut p = ext_up_reg();
    p.pub_key_operator = under(BlsSchemeKind::Basic);
    assert_eq!(check_up(&raw_legacy_state(bytes), p), Ok(()));
}

fn up_reg_payee_reuse() -> Verdict {
    // A payout to the state's owner key.
    let (fx, mut p) = up_reg();
    p.script_payout = p2pkh(*owner_signer().1.as_bytes()).into();
    check_up(&fx, p)
}

fn up_reg_payouts_evo() -> Verdict {
    up_reg_payouts(MnType::Evo, false, vec![payout(0x31, 6000), payout(0x32, 4000)])
}

/// An ExtAddr registrar update of a version-3 masternode of `mn_type` to `payouts`.
fn up_reg_payouts(mn_type: MnType, evo_shares: bool, payouts: Vec<PayoutShare>) -> Verdict {
    let (mut fx, _) = up_reg();
    fx.rules.evo_shares = evo_shares;
    let (_, _, mut p, _) = super::trivial::tests::ext_addr_payloads();
    p.pro_tx_hash = TxHash::from(*pro_tx(1).as_bytes());
    p.payouts = payouts;
    let fx = replaced(
        fx,
        dmn_with(1, mn_type, |d, c| {
            d.collateral = outpoint(UP_COLLATERAL, 0);
            c.version = 3;
            c.net_info = Arc::new(ext(&[]));
            c.key_id_owner = owner_signer().1;
            c.pub_key_operator = operator_key(1, BlsSchemeKind::Basic);
        }),
    );
    check_up(&fx, p)
}

fn up_reg_collateral() -> Verdict {
    let (mut fx, p) = up_reg();
    fx.coins = ModelCoins::default();
    check_up(&fx, p)
}

fn up_reg_collateral_dest() -> Verdict {
    let (mut fx, p) = up_reg();
    fx.coins =
        ModelCoins::default().with(outpoint(UP_COLLATERAL, 0), txout(1000 * COIN, vec![0x51]));
    check_up(&fx, p)
}

fn up_reg_collateral_reuse() -> Verdict {
    // The new voting key is the collateral's.
    let (fx, mut p) = up_reg();
    p.key_id_voting = collateral_signer().1;
    check_up(&fx, p)
}

fn up_reg_dup_key() -> Verdict {
    // Another masternode holds the new key under the same encoding: the probe at 1525 sees it
    // first under V24, so this is the pre-V24 site (1566).
    let (mut fx, p) = up_reg();
    fx.rules.v24 = false;
    let operator = p.pub_key_operator.clone();
    check_up(&fx.with_mn(mn(2, |_, c| c.pub_key_operator = operator)), p)
}

fn up_reg_key_not_same() -> Verdict {
    let (mut fx, p) = up_reg();
    fx.height = NOT_ENFORCED;
    check_up(&fx, p)
}

fn up_reg_inputs_hash() -> Verdict {
    let (fx, mut p) = up_reg();
    p.inputs_hash = Default::default();
    verdict(fx.check(&special(
        TxType::ProviderUpdateRegistrar,
        vec![txin([0x34; 32], 0)],
        vec![],
        &p,
    )))
}

/// [`up_reg`] signed by `key`.
fn signed_up_reg(key: &SecretKey) -> (Fixture, Tx) {
    let (mut fx, mut p) = up_reg();
    fx.check_sigs = true;
    let inputs = vec![txin([0x34; 32], 0)];
    p.inputs_hash = carried_inputs_hash(&inputs);
    p.vch_sig = sign(&payload_sig_hash(&p).expect("a preimage"), key);
    (fx, special(TxType::ProviderUpdateRegistrar, inputs, vec![], &p))
}

fn up_reg_sig() -> Verdict {
    let (fx, tx) = signed_up_reg(&signer(0x99).0);
    verdict(fx.check(&tx))
}

#[test]
fn owner_key_signs_the_update() {
    let (fx, tx) = signed_up_reg(&owner_signer().0);
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
}

// --- The shared masternode (§3.7–§3.9) ---

/// Core's `ProDisTestSetup` (`evo_sharedmn_tests.cpp` rc.4:633-745): three shares of 500, 300 and
/// 200 DASH, registered at 1000 with a 100-block early period and a 50 DASH penalty.
struct ProDis {
    refund: [Vec<u8>; 3],
    owners: [(SecretKey, KeyId); 3],
    list: MnList,
}

const REGISTERED_HEIGHT: i32 = 1000;
const EARLY_PERIOD: u32 = 100;
const PENALTY: i64 = 50 * COIN;
const FEE: i64 = 100_000;
const IN_EARLY: i32 = REGISTERED_HEIGHT + 50;
const AFTER_EARLY: i32 = REGISTERED_HEIGHT + EARLY_PERIOD as i32;
const SHARED_N: u8 = 5;

impl ProDis {
    fn new() -> ProDis {
        let refund = [p2pkh([0xa0; 20]), p2pkh([0xa1; 20]), p2pkh([0xa2; 20])];
        let owners = [signer(0xb0), signer(0xb1), signer(0xb2)];
        let shares: Vec<CollateralShare> = [500, 300, 200]
            .iter()
            .zip(&refund)
            .zip(&owners)
            .map(|((amount, refund), (_, owner))| CollateralShare {
                amount: amount * COIN,
                script_refund: refund.clone().into(),
                script_reward: vec![].into(),
                key_id_owner: *owner,
            })
            .collect();
        let mut list = MnList::scratch();
        let dmn = mn(SHARED_N, |d, c| {
            d.collateral = outpoint(*pro_tx(SHARED_N).as_bytes(), 0);
            c.version = 3;
            c.net_info = Arc::new(ext(&[]));
            c.key_id_owner = KeyId::default();
            c.shares = shares;
            c.registered_height = REGISTERED_HEIGHT;
            c.early_period_blocks = EARLY_PERIOD;
            c.early_penalty = PENALTY;
        });
        list.try_add(dmn, true).expect("a masternode");
        ProDis {
            refund,
            owners,
            list,
        }
    }

    fn shares(&self) -> &[CollateralShare] {
        &self.list.get(&pro_tx(SHARED_N)).expect("listed").cold.shares
    }

    /// `BuildTx`: non-actor refunds in share order with the pro-rata penalty (the remainder to
    /// the last), the actor output when positive, and the signatures.
    fn build(&self, actor: u16, penalty: i64, unanimous: bool) -> (Tx, ProDisTx) {
        let shares = self.shares();
        let total: i64 = shares
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != usize::from(actor))
            .map(|(_, s)| s.amount)
            .sum();
        let last = (0..shares.len()).rev().find(|i| *i != usize::from(actor)).expect("a share");
        let mut outputs = Vec::new();
        let mut distributed = 0;
        for (i, share) in shares.iter().enumerate() {
            if i == usize::from(actor) {
                continue;
            }
            let bonus = if i == last {
                penalty - distributed
            } else {
                // Shares of no weight: Core's builder throws as its check does, which
                // `prodis_throw` builds to reach.
                pro_rata_floor(penalty, share.amount, total).unwrap_or_default()
            };
            distributed += bonus;
            outputs.push(txout(share.amount + bonus, share.script_refund.as_bytes().to_vec()));
        }
        let actor_out = shares[usize::from(actor)].amount - penalty - FEE;
        if actor_out > 0 {
            outputs.push(txout(actor_out, self.refund[usize::from(actor)].clone()));
        }
        self.signed(actor, outputs, unanimous)
    }

    /// The dissolution by `actor` paying `outputs`, signed.
    fn signed(&self, actor: u16, outputs: Vec<TxOut>, unanimous: bool) -> (Tx, ProDisTx) {
        let p = ProDisTx {
            version: 1,
            pro_tx_hash: TxHash::from(*pro_tx(SHARED_N).as_bytes()),
            actor_index: actor,
            sigs: vec![],
        };
        let tx = tx_of(
            TxType::ProviderDissolve,
            vec![txin(*pro_tx(SHARED_N).as_bytes(), 0)],
            outputs,
            vec![],
        );
        self.sign(tx, p, unanimous)
    }

    /// `Sign`: every owner in share order, or the actor alone, over `MakeSignHash`.
    fn sign(&self, tx: Tx, mut p: ProDisTx, unanimous: bool) -> (Tx, ProDisTx) {
        let signers: Vec<&SecretKey> = if unanimous {
            self.owners.iter().map(|(k, _)| k).collect()
        } else {
            vec![&self.owners[usize::from(p.actor_index)].0]
        };
        let hash = pro_dis_sign_hash(&p, &tx, signers.len() as u8);
        p.sigs = signers.into_iter().map(|k| compact(&sign(&hash, k))).collect();
        (tx, p)
    }

    /// The shared masternode with its state edited by `f`.
    fn with_cold(mut self, f: impl FnOnce(&mut MnCold)) -> ProDis {
        let mut dmn = Dmn::clone(self.list.get(&pro_tx(SHARED_N)).expect("listed"));
        let mut cold = MnCold::clone(&dmn.cold);
        f(&mut cold);
        dmn.cold = Arc::new(crate::testkit::fresh(cold));
        self.list = MnList::scratch();
        self.list.try_add(Arc::new(dmn), true).expect("a masternode");
        self
    }

    /// `tx` with its raw transaction edited, then re-signed.
    fn edited(
        &self,
        tx: &Tx,
        p: ProDisTx,
        unanimous: bool,
        f: impl FnOnce(&mut Transaction),
    ) -> (Tx, ProDisTx) {
        let mut raw = tx.raw().clone();
        f(&mut raw);
        self.sign(Tx::new(raw), p, unanimous)
    }

    fn check(&self, tx: &Tx, p: &ProDisTx, height: i32) -> Verdict {
        check_pro_dis_tx_for_list(tx, p, &self.list, height, true)
    }
}

/// Core's `prodis_validation` (`evo_sharedmn_tests.cpp` rc.4:747-922), the accepting cases.
#[test]
fn prodis_validation_accepts() {
    let t = ProDis::new();
    // Unilateral with the penalty: valid in the early period and, monotone, ever after.
    let (tx, p) = t.build(2, PENALTY, false);
    assert_eq!(t.check(&tx, &p, IN_EARLY), Ok(()));
    assert_eq!(t.check(&tx, &p, AFTER_EARLY + 100_000), Ok(()));
    // Unilateral without the penalty: valid from the boundary.
    let (tx, p) = t.build(2, 0, false);
    assert_eq!(t.check(&tx, &p, AFTER_EARLY), Ok(()));
    // Unanimous without the penalty: valid at any height.
    let (tx, p) = t.build(0, 0, true);
    assert_eq!(t.check(&tx, &p, IN_EARLY), Ok(()));
    // The full penalty kept after the boundary: an overpayment exactly at the ceiling.
    let (tx, p) = t.build(2, PENALTY, false);
    assert_eq!(t.check(&tx, &p, AFTER_EARLY), Ok(()));
    // Unanimous bonuses beyond the ceiling are consented.
    let (tx, p) = t.build(0, PENALTY + 12_345, true);
    assert_eq!(t.check(&tx, &p, IN_EARLY), Ok(()));
    // A fee of exactly `MAX_FEE`.
    let (tx, p) = t.build(0, 0, true);
    let (tx, p) = t.edited(&tx, p, true, |raw| {
        raw.outputs.last_mut().expect("an output").value -= PRODIS_MAX_FEE - FEE;
    });
    assert_eq!(t.check(&tx, &p, IN_EARLY), Ok(()));
}

fn prodis_penalty_floor() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(2, 0, false);
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_bonus() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(2, PENALTY + 12_345, false);
    assert_reject(t.check(&tx, &p, AFTER_EARLY), S, BAD_PRODIS_BONUS);
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_fee() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let (tx, p) = t.edited(&tx, p, true, |raw| {
        raw.outputs.last_mut().expect("an output").value -= PRODIS_MAX_FEE;
    });
    t.check(&tx, &p, IN_EARLY)
}

/// The theft shape the fee ceiling closes: the actor output dropped, so the actor's whole share
/// is fee.
#[test]
fn prodis_dropped_actor_output_is_fee() {
    let t = ProDis::new();
    let (tx, p) = t.build(2, 0, false);
    let (tx, p) = t.edited(&tx, p, false, |raw| drop(raw.outputs.pop()));
    assert_reject(t.check(&tx, &p, AFTER_EARLY), S, BAD_PRODIS_FEE);
}

fn prodis_hash() -> Verdict {
    let t = ProDis::new();
    let (tx, mut p) = t.build(0, 0, true);
    p.pro_tx_hash = TxHash::from([0xee; 32]);
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_input() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let wrong = t.edited(&tx, p.clone(), true, |raw| raw.inputs[0].prevout.index = 1);
    assert_reject(t.check(&wrong.0, &wrong.1, IN_EARLY), S, BAD_PRODIS_INPUT);
    let extra = t.edited(&tx, p.clone(), true, |raw| raw.inputs.push(txin([0xee; 32], 0)));
    assert_reject(t.check(&extra.0, &extra.1, IN_EARLY), S, BAD_PRODIS_INPUT);
    let (tx, p) = t.edited(&tx, p, true, |raw| raw.inputs[0].script_sig = vec![0x51].into());
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_actor() -> Verdict {
    let t = ProDis::new();
    let (tx, mut p) = t.build(0, 0, true);
    p.actor_index = 3;
    let (tx, p) = t.sign(tx, p, true);
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_sig_count() -> Verdict {
    let t = ProDis::new();
    let (tx, mut p) = t.build(0, 0, true);
    p.sigs.truncate(2);
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_sig() -> Verdict {
    let t = ProDis::new();
    // Signed by a non-actor share.
    let (tx, mut p) = t.build(2, PENALTY, false);
    let hash = pro_dis_sign_hash(&p, &tx, 1);
    p.sigs = vec![compact(&sign(&hash, &t.owners[0].0))];
    assert_reject(t.check(&tx, &p, IN_EARLY), S, BAD_PRODIS_SIG);
    // Unanimous signatures out of share order.
    let (tx, mut p) = t.build(0, 0, true);
    p.sigs.swap(0, 1);
    assert_reject(t.check(&tx, &p, IN_EARLY), S, BAD_PRODIS_SIG);
    // Not malleable: the actor's signature of a unanimous dissolution committed to three
    // signatures, so it fails as a one-signature transaction.
    let (tx, mut p) = t.build(0, 0, true);
    p.sigs.truncate(1);
    t.check(&tx, &p, AFTER_EARLY)
}

fn prodis_payee() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let swapped = t.edited(&tx, p.clone(), true, |raw| raw.outputs.swap(0, 1));
    assert_reject(t.check(&swapped.0, &swapped.1, IN_EARLY), S, BAD_PRODIS_PAYEE);
    let refund = t.refund[0].clone();
    let (tx, p) = t.edited(&tx, p, true, |raw| raw.outputs[0].script_pubkey = refund.into());
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_payee_count() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let refund = t.refund[0].clone();
    let (tx, p) = t.edited(&tx, p, true, |raw| raw.outputs.push(txout(1, refund)));
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_penalty_floor_underpaid() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(2, PENALTY, false);
    let (tx, p) = t.edited(&tx, p, false, |raw| raw.outputs[0].value -= 1);
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_actor_output_zero() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let refund = t.refund[0].clone();
    let (tx, p) = t.edited(&tx, p, true, |raw| {
        raw.outputs.pop();
        raw.outputs.push(txout(0, refund));
    });
    t.check(&tx, &p, IN_EARLY)
}

fn prodis_actor_payee() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let (tx, p) = t.edited(&tx, p, true, |raw| {
        raw.outputs.last_mut().expect("an output").script_pubkey = p2pkh([1; 20]).into()
    });
    t.check(&tx, &p, IN_EARLY)
}

/// The per-recipient floors drop the remainders; the sum rule puts them on an output: a
/// unilateral penalty of 101 duffs over weights 300 and 200 floors to 60 + 40.
fn prodis_penalty_sum() -> Verdict {
    let t = ProDis::new().with_cold(|c| c.early_penalty = 101);
    let shares = t.shares();
    let outputs = vec![
        txout(shares[1].amount + 60, t.refund[1].clone()),
        txout(shares[2].amount + 40, t.refund[2].clone()),
        txout(shares[0].amount - 101 - FEE, t.refund[0].clone()),
    ];
    let (tx, p) = t.signed(0, outputs, false);
    t.check(&tx, &p, IN_EARLY)
}

/// `ProRataFloor` over `arith_uint256`: exact for every `u64` input, the low 64 bits as a
/// `CAmount`; a zero divisor throws (`None`).
#[test]
fn pro_rata_floor_is_cores() {
    assert_eq!(pro_rata_floor(PENALTY, 300 * COIN, 500 * COIN), Some(30 * COIN));
    assert_eq!(pro_rata_floor(101, 3, 5), Some(60));
    // A product past 64 bits.
    assert_eq!(pro_rata_floor(i64::MAX, i64::MAX, i64::MAX), Some(i64::MAX));
    assert_eq!(pro_rata_floor(i64::MAX, 4, 2), Some(-2));
    // Negative amounts read as `uint64_t`.
    assert_eq!(pro_rata_floor(-1, 1, 1), Some(-1));
    assert_eq!(pro_rata_floor(-1, 2, 2), Some(-1));
    assert_eq!(pro_rata_floor(1, 1, -1), Some(0));
    assert_eq!(pro_rata_floor(5, 7, 0), None);
}

/// Core's throw site, through the dispatcher: a dissolution whose non-actor shares weigh nothing
/// divides by zero, which Core's catch turns into `failed-check-special-tx`
/// (`specialtxman.cpp` rc.4:267-271).
fn prodis_throw() -> Verdict {
    let t = ProDis::new().with_cold(|c| {
        c.shares[1].amount = 0;
        c.shares[2].amount = 0;
    });
    let (tx, p) = t.build(0, 0, false);
    let mut fx = dis_fixture(&t);
    fx.height = IN_EARLY as u32;
    verdict(fx.check(&dis_tx(&tx, &p)))
}

/// `CheckProDisTx` (§3.7) through the dispatcher: the payload carries the fixture's
/// dissolution.
fn dis_fixture(t: &ProDis) -> Fixture {
    let mut fx = Fixture::v24();
    fx.list = t.list.clone();
    fx
}

fn dis_tx(tx: &Tx, p: &ProDisTx) -> Tx {
    let raw = tx.raw().clone();
    special(raw.tx_type, raw.inputs, raw.outputs, p)
}

fn prodis_too_early() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let mut fx = dis_fixture(&t);
    fx.rules.v24 = false;
    verdict(fx.check(&dis_tx(&tx, &p)))
}

fn prodis_not_listed() -> Verdict {
    let t = ProDis::new();
    let (tx, mut p) = t.build(0, 0, true);
    p.pro_tx_hash = TxHash::from([0xee; 32]);
    verdict(dis_fixture(&t).check(&dis_tx(&tx, &p)))
}

fn prodis_not_shared() -> Verdict {
    let t = ProDis::new();
    let (tx, p) = t.build(0, 0, true);
    let mut fx = dis_fixture(&t);
    fx.list = MnList::scratch();
    let fx =
        fx.with_mn(mn(SHARED_N, |d, _| d.collateral = outpoint(*pro_tx(SHARED_N).as_bytes(), 0)));
    verdict(fx.check(&dis_tx(&tx, &p)))
}

/// The dispatcher's dissolution is checked at `pindexPrev`'s height plus one.
#[test]
fn prodis_spend_height_is_the_block() {
    let t = ProDis::new();
    let (tx, p) = t.build(2, 0, false);
    let tx = dis_tx(&tx, &p);
    let mut fx = dis_fixture(&t);
    fx.height = AFTER_EARLY as u32;
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
    fx.height = AFTER_EARLY as u32 - 1;
    assert_reject(verdict(fx.check(&tx)), S, BAD_PRODIS_PENALTY_FLOOR);
}

/// The shared masternode of [`ProDis`] in the context, with signatures checked.
fn share_fixture() -> (Fixture, ProDis) {
    let t = ProDis::new();
    let mut fx = dis_fixture(&t);
    fx.check_sigs = true;
    (fx, t)
}

const SHARE_INPUT: [u8; 32] = [0x44; 32];

/// A reward update of share 1, its `inputsHash` set, unsigned.
fn share_payload(f: impl FnOnce(&mut ProUpShareTx)) -> ProUpShareTx {
    let mut p = pro_up_share();
    p.pro_tx_hash = TxHash::from(*pro_tx(SHARED_N).as_bytes());
    p.share_index = 1;
    // Clear of masternode 5's voting key, `[5; 20]`.
    p.script_reward = p2pkh([0x5e; 20]).into();
    p.inputs_hash = carried_inputs_hash(&[txin(SHARE_INPUT, 0)]);
    f(&mut p);
    p
}

fn share_tx(p: &ProUpShareTx) -> Tx {
    special(TxType::ProviderUpdateShare, vec![txin(SHARE_INPUT, 0)], vec![], p)
}

/// [`share_payload`] signed by `key`.
fn up_share(key: &SecretKey, f: impl FnOnce(&mut ProUpShareTx)) -> Tx {
    let mut p = share_payload(f);
    p.vch_sig = sign(&payload_sig_hash(&p).expect("a preimage"), key);
    share_tx(&p)
}

#[test]
fn share_owner_updates_its_reward() {
    let (fx, t) = share_fixture();
    assert_eq!(verdict(fx.check(&up_share(&t.owners[1].0, |_| {}))), Ok(()));
}

fn share_too_early() -> Verdict {
    let (mut fx, t) = share_fixture();
    fx.rules.v24 = false;
    verdict(fx.check(&up_share(&t.owners[1].0, |_| {})))
}

fn share_hash() -> Verdict {
    let (fx, t) = share_fixture();
    verdict(fx.check(&up_share(&t.owners[1].0, |p| p.pro_tx_hash = TxHash::from([0xee; 32]))))
}

fn share_not_shared() -> Verdict {
    let (mut fx, t) = share_fixture();
    fx.list = MnList::scratch();
    let fx = fx.with_mn(mn(SHARED_N, |_, _| {}));
    verdict(fx.check(&up_share(&t.owners[1].0, |_| {})))
}

fn share_index() -> Verdict {
    let (fx, t) = share_fixture();
    verdict(fx.check(&up_share(&t.owners[1].0, |p| p.share_index = 3)))
}

fn share_payee_reuse() -> Verdict {
    let (fx, t) = share_fixture();
    let owner = *t.owners[2].1.as_bytes();
    let voting = t.list.get(&pro_tx(SHARED_N)).expect("listed").cold.key_id_voting;
    let to_voting =
        up_share(&t.owners[1].0, |p| p.script_reward = p2pkh(*voting.as_bytes()).into());
    assert_reject(verdict(fx.check(&to_voting)), S, BAD_PROUPSHARE_PAYEE_REUSE);
    verdict(fx.check(&up_share(&t.owners[1].0, |p| p.script_reward = p2pkh(owner).into())))
}

fn share_inputs_hash() -> Verdict {
    let (mut fx, _) = share_fixture();
    fx.check_sigs = false;
    verdict(fx.check(&share_tx(&share_payload(|p| p.inputs_hash = Default::default()))))
}

fn share_sig() -> Verdict {
    let (fx, t) = share_fixture();
    verdict(fx.check(&up_share(&t.owners[0].0, |_| {})))
}

/// A shared registrar update of the [`ProDis`] masternode, signed by `keys` in order.
fn up_shared_reg(keys: &[&SecretKey], f: impl FnOnce(&mut ProUpSharedRegTx)) -> Tx {
    let mut p = pro_up_shared_reg();
    p.pro_tx_hash = TxHash::from(*pro_tx(SHARED_N).as_bytes());
    p.sigs = vec![compact(&[0; 65]); 3];
    f(&mut p);
    let inputs = vec![txin([0x45; 32], 0)];
    p.inputs_hash = carried_inputs_hash(&inputs);
    let hash = payload_sig_hash(&p).expect("a preimage");
    for (sig, key) in p.sigs.iter_mut().zip(keys) {
        *sig = compact(&sign(&hash, key));
    }
    special(TxType::ProviderUpdateSharedRegistrar, inputs, vec![], &p)
}

fn owner_keys(t: &ProDis) -> Vec<&SecretKey> {
    t.owners.iter().map(|(k, _)| k).collect()
}

#[test]
fn every_share_owner_updates_the_registrar() {
    let (fx, t) = share_fixture();
    assert_eq!(verdict(fx.check(&up_shared_reg(&owner_keys(&t), |_| {}))), Ok(()));
}

fn shared_reg_too_early() -> Verdict {
    let (mut fx, t) = share_fixture();
    fx.rules.v24 = false;
    verdict(fx.check(&up_shared_reg(&owner_keys(&t), |_| {})))
}

fn shared_reg_hash() -> Verdict {
    let (fx, t) = share_fixture();
    verdict(fx.check(&up_shared_reg(&owner_keys(&t), |p| p.pro_tx_hash = TxHash::from([0xee; 32]))))
}

fn shared_reg_not_shared() -> Verdict {
    let (mut fx, t) = share_fixture();
    fx.list = MnList::scratch();
    let fx = fx.with_mn(mn(SHARED_N, |_, _| {}));
    verdict(fx.check(&up_shared_reg(&owner_keys(&t), |_| {})))
}

fn shared_reg_sig_count() -> Verdict {
    let (fx, t) = share_fixture();
    verdict(fx.check(&up_shared_reg(&owner_keys(&t), |p| p.sigs.truncate(2))))
}

fn shared_reg_dup_key() -> Verdict {
    // Ungated: another masternode holds the key under the other scheme, before V24's checks
    // matter.
    let (fx, t) = share_fixture();
    let key = pro_up_shared_reg().pub_key_operator;
    let fx = fx.with_mn(legacy_holder(1, &key));
    verdict(fx.check(&up_shared_reg(&owner_keys(&t), |_| {})))
}

fn shared_reg_payee_reuse() -> Verdict {
    // The new voting key is a share's refund key.
    let (fx, t) = share_fixture();
    verdict(
        fx.check(&up_shared_reg(&owner_keys(&t), |p| p.key_id_voting = KeyId::from([0xa1; 20]))),
    )
}

fn shared_reg_inputs_hash() -> Verdict {
    let (mut fx, _) = share_fixture();
    fx.check_sigs = false;
    let mut p = pro_up_shared_reg();
    p.pro_tx_hash = TxHash::from(*pro_tx(SHARED_N).as_bytes());
    p.sigs = vec![compact(&[0; 65]); 3];
    verdict(fx.check(&special(
        TxType::ProviderUpdateSharedRegistrar,
        vec![txin([0x45; 32], 0)],
        vec![],
        &p,
    )))
}

fn shared_reg_sig() -> Verdict {
    let (fx, t) = share_fixture();
    let mut keys = owner_keys(&t);
    keys.swap(0, 1);
    verdict(fx.check(&up_shared_reg(&keys, |_| {})))
}

// --- The covenant (§3.17) ---

/// Core's `shared_collateral_template` (`evo_sharedmn_tests.cpp` rc.4:37-62): the exact seven
/// bytes, never a prefix or a near miss.
#[test]
fn shared_collateral_template() {
    assert_eq!(SHARED_COLLATERAL_SCRIPT, [0x04, 0x44, 0x53, 0x48, 0x43, 0x75, 0x51]);
    assert!(is_shared_collateral_script(&SHARED_COLLATERAL_SCRIPT));
    let mut extended = SHARED_COLLATERAL_SCRIPT.to_vec();
    extended.push(0x61);
    assert!(!is_shared_collateral_script(&extended));
    assert!(!is_shared_collateral_script(&SHARED_COLLATERAL_SCRIPT[..6]));
    let mut wrong_tag = SHARED_COLLATERAL_SCRIPT;
    wrong_tag[4] = b'D';
    assert!(!is_shared_collateral_script(&wrong_tag));
    assert!(!is_shared_collateral_script(&[]));
}

const TEMPLATE_COIN: [u8; 32] = [0x66; 32];

fn template_view() -> ModelCoins {
    ModelCoins::default().with(outpoint(TEMPLATE_COIN, 0), template_collateral(1000 * COIN))
}

/// A plain transaction spending `(coin, index)`.
fn spend_of(coin: [u8; 32], index: u32) -> Tx {
    tx_of(TxType::Spend, vec![txin(coin, index)], vec![], vec![])
}

fn spends(tx: &Tx, coins: &ModelCoins) -> Verdict {
    shared_collateral_spends(tx, coins).expect("no fault")
}

fn covenant_spend() -> Verdict {
    spends(&spend_of(TEMPLATE_COIN, 0), &template_view())
}

#[test]
fn covenant_spends() {
    let view = template_view();
    // Only a ProDisTx spends a template output.
    let dissolve = tx_of(TxType::ProviderDissolve, vec![txin(TEMPLATE_COIN, 0)], vec![], vec![]);
    assert_eq!(spends(&dissolve, &view), Ok(()));
    // A ProDisTx type in a classic version is not one.
    let mut raw = dissolve.raw().clone();
    raw.version = 2;
    assert_reject(spends(&Tx::new(raw), &view), C, BAD_SHARED_COLLATERAL_SPEND);
    // Other coins, and coins not in the view, are not template outputs.
    let other = spend_of([0x67; 32], 0);
    assert_eq!(spends(&other, &view), Ok(()));
    let unspent_elsewhere =
        ModelCoins::default().with(outpoint(TEMPLATE_COIN, 0), txout(COIN, p2pkh([1; 20])));
    assert_eq!(spends(&spend_of(TEMPLATE_COIN, 0), &unspent_elsewhere), Ok(()));
    // A coinbase spends nothing.
    let coinbase = spend_of([0; 32], u32::MAX);
    assert!(coinbase.is_coinbase());
    assert_eq!(spends(&coinbase, &view), Ok(()));
    assert_eq!(shared_collateral_spends(&coinbase, &FailingCoins), Ok(Ok(())));
    // A failed read faults.
    assert_eq!(
        shared_collateral_spends(&spend_of(TEMPLATE_COIN, 0), &FailingCoins),
        Err(SpecialTxFault::Coins("read failure"))
    );
}

fn outputs_of(tx: &Tx) -> Verdict {
    shared_collateral_template_outputs(tx, tx.payload())
}

/// A template output outside a ProRegTx.
fn covenant_create() -> Verdict {
    let tx = tx_of(TxType::Spend, vec![], vec![template_collateral(COIN)], vec![]);
    outputs_of(&tx)
}

/// A ProRegTx whose payload does not decode whole.
fn covenant_create_undecoded() -> Verdict {
    let mut bytes = encoded(&shared_reg());
    bytes.push(0);
    let tx = tx_of(TxType::ProviderRegister, vec![], vec![template_collateral(COIN)], bytes);
    outputs_of(&tx)
}

/// A shared registration's template output other than its collateral.
fn covenant_create_not_collateral() -> Verdict {
    let mut p = shared_reg();
    outputs_of(&reg_tx(&mut p, vec![int_collateral(), template_collateral(1000 * COIN)]))
}

#[test]
fn covenant_template_outputs() {
    // A shared registration's own collateral output.
    let mut p = shared_reg();
    let tx = reg_tx(&mut p, vec![template_collateral(1000 * COIN)]);
    assert_eq!(outputs_of(&tx), Ok(()));
    // A registration that is not shared, or with an external collateral.
    let mut plain = int_reg();
    let tx = reg_tx(&mut plain, vec![template_collateral(1000 * COIN)]);
    assert_reject(outputs_of(&tx), C, BAD_SHARED_COLLATERAL_CREATE);
    let mut external = shared_reg();
    external.collateral_hash = TxHash::from([1; 32]);
    let tx = reg_tx(&mut external, vec![template_collateral(1000 * COIN)]);
    assert_reject(outputs_of(&tx), C, BAD_SHARED_COLLATERAL_CREATE);
    // No template output: nothing to check.
    let tx = tx_of(TxType::Spend, vec![], vec![txout(COIN, p2pkh([1; 20]))], vec![]);
    assert_eq!(outputs_of(&tx), Ok(()));
}

// --- Coins views (D13, D26) ---

/// The mempool binding's two views (D13): the check reads `CoinsTip()`, the covenant the mempool
/// input cache, so an unconfirmed parent is visible to the second only.
#[test]
fn coins_views_d13() {
    // (a) A registration whose external collateral is an unconfirmed output.
    let (fx, mut p) = ext_reg();
    let tip = ModelCoins::default();
    let tx = reg_tx(&mut p, vec![]);
    let at_tip = fx.with_view(&tip, |cx| check_special_tx_inner(&tx, tx.payload(), cx));
    assert_reject(verdict(at_tip), S, BAD_PROTX_COLLATERAL);
    assert_eq!(verdict(fx.check(&tx)), Ok(()), "the cache view holds it");
    // (b) A non-dissolution spending an unconfirmed registration's template output.
    let spend = spend_of(TEMPLATE_COIN, 0);
    assert_reject(spends(&spend, &template_view()), C, BAD_SHARED_COLLATERAL_SPEND);
    assert_eq!(spends(&spend, &ModelCoins::default()), Ok(()), "the tip view lacks it");
}

/// Register quirk 26 (EV1 D40; `specialtxman.cpp` rc.4:1245-1249): an external collateral the
/// view lacks, as one created earlier in the same block is from `pindexPrev`'s, is
/// `bad-protx-collateral`; the next block's view holds it. Core's block path confirms it on the
/// oracle: a block funding the collateral and registering on it is refused in Core's check loop
/// and ours alike (`node-l1/tests/l1_specialtx.rs::same_block_collateral_is_refused_quirk_26`,
/// EV1-3b).
#[test]
fn quirk_26_collateral_from_pprev_utxo() {
    let funding = tx_of(
        TxType::Spend,
        vec![txin([0x21; 32], 0)],
        vec![txout(1000 * COIN, collateral_script())],
        vec![],
    );
    let created = outpoint(*funding.txid().as_bytes(), 0);
    let (mut fx, mut p) = ext_reg();
    p.collateral_hash = created.hash;
    p.collateral_index = created.index;
    let tx = reg_tx(&mut p, vec![]);
    fx.coins = ModelCoins::default();
    assert_reject(verdict(fx.check(&tx)), S, BAD_PROTX_COLLATERAL);
    fx.coins = ModelCoins::default().with(created, funding.outputs()[0].clone());
    assert_eq!(verdict(fx.check(&tx)), Ok(()));
}

// --- Operator-key equality (D17) ---

const D17: &str = include_str!("data/d17-lazy-eq.txt");

/// A corpus class: its input and its eager outcomes under the legacy and basic flags.
struct Class {
    input: [u8; 48],
    legacy: String,
    basic: String,
}

fn unhex48(s: &str) -> [u8; 48] {
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect();
    bytes.try_into().expect("48 bytes")
}

fn flag(s: &str) -> BlsSchemeKind {
    match s {
        "L" => BlsSchemeKind::Legacy,
        "B" => BlsSchemeKind::Basic,
        _ => panic!("bad scheme flag {s}"),
    }
}

/// A corpus byte tag: `raw`, `zero`, or `XY` (the decode under X serialized under Y).
fn tagged(class: &Class, tag: &str) -> [u8; 48] {
    match tag {
        "raw" => class.input,
        "zero" => [0; 48],
        _ => {
            let (x, y) = tag.split_at(1);
            let outcome = if x == "L" {
                &class.legacy
            } else {
                &class.basic
            };
            let other = outcome.strip_prefix("V:").unwrap_or_else(|| panic!("no {tag}"));
            if x == y {
                class.input
            } else {
                unhex48(other)
            }
        }
    }
}

/// The input of the corpus class `name`.
fn corpus_input(name: &str) -> [u8; 48] {
    let prefix = format!("class {name} ");
    let line = D17.lines().find_map(|l| l.strip_prefix(prefix.as_str()));
    unhex48(line.unwrap_or_else(|| panic!("no class {name}")).split(' ').next().expect("hex"))
}

/// A lazy key built as the corpus's ops say: `unser:S` (read under S), `new:S` (default),
/// `set:XY` (the decode under X, or the invalid key, set under Y), then any `get`.
fn lazy(class: &Class, ops: &str) -> OperatorKey {
    let mut ops = ops.split(',');
    let first = ops.next().expect("a constructor");
    let (name, arg) = first.split_once(':').expect("an argument");
    let key = match name {
        "unser" => BlsLazyPk::from_bytes(&class.input, BlsSchemeSource::Version(flag(arg))),
        "new" => BlsLazyPk::new(flag(arg)),
        "set" => {
            let (x, y) = arg.split_at(1);
            let value =
                BlsValue::from_canonical(&class.input, flag(x)).unwrap_or_else(BlsValue::invalid);
            BlsLazyPk::from_value(value, flag(y))
        }
        _ => panic!("bad constructor {first}"),
    };
    for op in ops {
        assert_eq!(op, "get", "bad pair op");
        drop(key.get());
    }
    key
}

/// D17's eq half: `<OperatorKey as PartialEq>::eq`, the function `key_changed` reads, over every
/// lazy public-key pair of the BS12 corpus plus its classes' pairs with a Set-basic key (`set:BB`,
/// `set:LB`, probed with the corpus's probe), in both orders on fresh keys, with each side's
/// serialization afterwards. The serialization is the flags' observable: a failed decode clears
/// `bufValid`, so the key then writes zeros. The pairs were probed at rc.3; `src/bls/` is the same
/// at rc.4.
#[test]
fn operator_key_equality_is_cores_d17() {
    let mut classes = BTreeMap::new();
    let mut pairs = 0;
    for line in D17.lines().filter(|l| !l.starts_with('#')) {
        let f: Vec<&str> = line.split(' ').collect();
        match f[0] {
            "class" => {
                let class = Class {
                    input: unhex48(f[2]),
                    legacy: f[3].to_owned(),
                    basic: f[4].to_owned(),
                };
                classes.insert(f[1].to_owned(), class);
            }
            "pair" => {
                let (a, b) = (&classes[f[1]], &classes[f[3]]);
                // `a == b`, then `b == a` on fresh keys.
                for (reversed, at) in [(false, 5), (true, 8)] {
                    let (x, y) = (lazy(a, f[2]), lazy(b, f[4]));
                    let (lhs, rhs) = if reversed {
                        (&y, &x)
                    } else {
                        (&x, &y)
                    };
                    let eq = lhs == rhs;
                    assert_eq!(eq, f[at] == "1", "{line} reversed {reversed}");
                    assert_eq!(x.to_bytes(), tagged(a, f[at + 1]), "{line} {reversed}: a after");
                    assert_eq!(y.to_bytes(), tagged(b, f[at + 2]), "{line} {reversed}: b after");
                }
                pairs += 1;
            }
            other => panic!("bad line kind {other}"),
        }
    }
    assert_eq!(pairs, 1792);
    // The same object, which the corpus's fresh pairs cannot express.
    let key = operator_key(1, BlsSchemeKind::Basic);
    assert!(key == key);
}

// --- Census closure ---

/// A reject vector: the name, Core's class and reason, and our verdict.
type Case = (&'static str, TxValidationResult, &'static str, fn() -> Verdict);

/// Every reject site EV1-3 and EV1-4 land (§3.2–§3.9, §3.17), each by a vector.
const CASES: &[Case] = &[
    ("dispatch: dip3 inactive", S, BAD_TX_TYPE_DIP3_INACTIVE, dispatch_dip3_inactive),
    ("dispatch: unknown type", S, BAD_TX_TYPE_CHECK, dispatch_unknown_type),
    ("dispatch: throw", C, FAILED_CHECK_SPECIAL_TX, prodis_throw),
    ("reg 1220", C, BAD_PROTX_VERSION_DISALLOWED, reg_version_disallowed),
    ("reg 1225", S, BAD_PROTX_SHARES_EVO, reg_shares_evo),
    ("reg 1228", S, BAD_PROTX_PAYOUTS_EVO, reg_payouts_evo),
    ("reg 1248 missing", S, BAD_PROTX_COLLATERAL, reg_collateral_missing),
    ("reg 1248 value", S, BAD_PROTX_COLLATERAL, reg_collateral_value),
    ("reg 1252", S, BAD_PROTX_COLLATERAL_DEST, reg_collateral_dest),
    ("reg 1259", S, BAD_PROTX_COLLATERAL_PKH, reg_collateral_pkh),
    ("reg 1265", S, BAD_PROTX_COLLATERAL_INDEX, reg_collateral_index),
    ("reg 1268", S, BAD_PROTX_COLLATERAL, reg_internal_collateral_value),
    ("reg 1274", S, BAD_PROTX_SHARES_COLLATERAL_SCRIPT, reg_shares_collateral_script),
    ("reg 1277", S, BAD_PROTX_COLLATERAL_DEST, reg_internal_collateral_dest),
    ("reg key safe", S, BAD_PROTX_COLLATERAL_REUSE, reg_collateral_reuse),
    ("reg key safe payouts", S, BAD_PROTX_PAYEE_REUSE, reg_payee_reuse),
    ("reg 1295", S, BAD_PROTX_DUP_NETINFO_ENTRY, reg_dup_netinfo_entry),
    ("reg 1317 owner", S, BAD_PROTX_DUP_KEY, reg_dup_owner_key),
    ("reg 1317 operator", S, BAD_PROTX_DUP_KEY, reg_dup_operator_key),
    ("reg 1310 share owner", S, BAD_PROTX_DUP_KEY, reg_dup_share_owner_key),
    ("reg 1314 shared operator", S, BAD_PROTX_DUP_KEY, reg_shared_dup_operator_key),
    ("reg 1333", S, BAD_PROTX_DUP_KEY, reg_dup_key_any_scheme),
    ("reg 1339", S, BAD_PROTX_DUP_PLATFORMNODEID, reg_dup_platform_node_id),
    ("reg 1345", S, BAD_PROTX_KEY_NOT_SAME, reg_key_not_same),
    ("reg inputs hash", C, BAD_PROTX_INPUTS_HASH, reg_inputs_hash),
    ("reg 1364", C, BAD_PROTX_SIG, reg_internal_sig),
    ("reg string sig", C, BAD_PROTX_SIG, reg_external_sig),
    ("reg 1374", S, BAD_PROTX_SHARES_SIG, reg_shares_sig),
    ("upreg 1501", C, BAD_PROTX_HASH, up_reg_hash),
    ("upreg 1507", S, BAD_PROTX_SHARED_MN, up_reg_shared_mn),
    ("upreg downgrade", C, BAD_PROTX_VERSION_DOWNGRADE, up_reg_version_downgrade),
    ("upreg 1525", S, BAD_PROTX_DUP_KEY, up_reg_dup_key_any_scheme),
    ("upreg dup key, legacy state raised", S, BAD_PROTX_DUP_KEY, up_reg_dup_key_migration),
    ("upreg dup key before payouts", S, BAD_PROTX_DUP_KEY, up_reg_dup_key_before_payouts),
    ("upreg dup key before migration", S, BAD_PROTX_DUP_KEY, up_reg_dup_key_before_migration),
    ("upreg 1536", S, BAD_PROTX_NETINFO_VERSION, up_reg_migration),
    ("upreg payout list", S, BAD_PROTX_PAYEE_REUSE, up_reg_payee_reuse),
    ("upreg 1544", S, BAD_PROTX_PAYOUTS_EVO, up_reg_payouts_evo),
    ("upreg 1550", C, BAD_PROTX_COLLATERAL, up_reg_collateral),
    ("upreg 1556", C, BAD_PROTX_COLLATERAL_DEST, up_reg_collateral_dest),
    ("upreg key safe", S, BAD_PROTX_COLLATERAL_REUSE, up_reg_collateral_reuse),
    ("upreg 1566", S, BAD_PROTX_DUP_KEY, up_reg_dup_key),
    ("upreg 1574", S, BAD_PROTX_KEY_NOT_SAME, up_reg_key_not_same),
    ("upreg inputs hash", C, BAD_PROTX_INPUTS_HASH, up_reg_inputs_hash),
    ("upreg sig", C, BAD_PROTX_SIG, up_reg_sig),
    ("upserv 1401", C, BAD_PROTX_HASH, updates::serv_hash),
    ("upserv 1409", C, BAD_PROTX_TYPE_MISMATCH, updates::serv_type_mismatch),
    ("upserv 1412", C, BAD_PROTX_VERSION_DOWNGRADE, updates::serv_version_downgrade),
    ("upserv 1420", C, BAD_PROTX_VERSION_DISALLOWED, updates::serv_version_disallowed),
    ("upserv 1427", S, BAD_PROTX_NETINFO_VERSION, updates::serv_migration),
    ("upserv 1437", S, BAD_PROTX_DUP_KEY, updates::serv_dup_key_migration),
    ("upserv 1445", S, BAD_PROTX_DUP_NETINFO_ENTRY, updates::serv_dup_netinfo_entry),
    ("upserv 1461", S, BAD_PROTX_DUP_PLATFORMNODEID, updates::serv_dup_platform_node_id),
    ("upserv 1468", S, BAD_PROTX_OPERATOR_PAYEE, updates::serv_operator_payee_reward),
    ("upserv 1471", S, BAD_PROTX_OPERATOR_PAYEE, updates::serv_operator_payee_script),
    ("upserv inputs hash", C, BAD_PROTX_INPUTS_HASH, updates::serv_inputs_hash),
    ("upserv 1480", C, BAD_PROTX_SIG, updates::serv_sig),
    ("uprev 1603", C, BAD_PROTX_HASH, updates::rev_hash),
    ("uprev 1606", C, BAD_PROTX_VERSION_DOWNGRADE, updates::rev_version_downgrade),
    ("uprev inputs hash", C, BAD_PROTX_INPUTS_HASH, updates::rev_inputs_hash),
    ("uprev 1615", C, BAD_PROTX_SIG, updates::rev_sig),
    ("upshare 1633", C, BAD_PROUPSHARE_TOO_EARLY, share_too_early),
    ("upshare hash", S, BAD_PROUPSHARE_HASH, share_hash),
    ("upshare not shared", S, BAD_PROUPSHARE_NOT_SHARED, share_not_shared),
    ("upshare index", S, BAD_PROUPSHARE_INDEX, share_index),
    ("upshare payee reuse", S, BAD_PROUPSHARE_PAYEE_REUSE, share_payee_reuse),
    ("upshare inputs hash", C, BAD_PROTX_INPUTS_HASH, share_inputs_hash),
    ("upshare 1672", S, BAD_PROUPSHARE_SIG, share_sig),
    ("upsharedreg too early", C, BAD_PROUPSHAREDREG_TOO_EARLY, shared_reg_too_early),
    ("upsharedreg hash", S, BAD_PROUPSHAREDREG_HASH, shared_reg_hash),
    ("upsharedreg not shared", S, BAD_PROUPSHAREDREG_NOT_SHARED, shared_reg_not_shared),
    ("upsharedreg sig count", S, BAD_PROUPSHAREDREG_SIG_COUNT, shared_reg_sig_count),
    ("upsharedreg dup key", S, BAD_PROTX_DUP_KEY, shared_reg_dup_key),
    ("upsharedreg payee reuse", S, BAD_PROUPSHAREDREG_PAYEE_REUSE, shared_reg_payee_reuse),
    ("upsharedreg inputs hash", C, BAD_PROTX_INPUTS_HASH, shared_reg_inputs_hash),
    ("upsharedreg sig", S, BAD_PROUPSHAREDREG_SIG, shared_reg_sig),
    ("prodis 1859", C, BAD_PRODIS_TOO_EARLY, prodis_too_early),
    ("prodis 1870", S, BAD_PRODIS_HASH, prodis_not_listed),
    ("prodis for list hash", S, BAD_PRODIS_HASH, prodis_hash),
    ("prodis not shared", S, BAD_PRODIS_NOT_SHARED, prodis_not_shared),
    ("prodis input", S, BAD_PRODIS_INPUT, prodis_input),
    ("prodis actor", S, BAD_PRODIS_ACTOR, prodis_actor),
    ("prodis sig count", S, BAD_PRODIS_SIG_COUNT, prodis_sig_count),
    ("prodis payee count", S, BAD_PRODIS_PAYEE_COUNT, prodis_payee_count),
    ("prodis payee", S, BAD_PRODIS_PAYEE, prodis_payee),
    ("prodis actor payee", S, BAD_PRODIS_PAYEE, prodis_actor_payee),
    ("prodis penalty floor", S, BAD_PRODIS_PENALTY_FLOOR, prodis_penalty_floor),
    (
        "prodis penalty floor, underpaid",
        S,
        BAD_PRODIS_PENALTY_FLOOR,
        prodis_penalty_floor_underpaid,
    ),
    ("prodis actor output zero", S, BAD_PRODIS_ACTOR_OUTPUT_ZERO, prodis_actor_output_zero),
    ("prodis penalty sum", S, BAD_PRODIS_PENALTY_SUM, prodis_penalty_sum),
    ("prodis fee", S, BAD_PRODIS_FEE, prodis_fee),
    ("prodis bonus", S, BAD_PRODIS_BONUS, prodis_bonus),
    ("prodis sig", S, BAD_PRODIS_SIG, prodis_sig),
    ("covenant 1886", C, BAD_SHARED_COLLATERAL_SPEND, covenant_spend),
    ("covenant 1900 type", C, BAD_SHARED_COLLATERAL_CREATE, covenant_create),
    ("covenant 1900 payload", C, BAD_SHARED_COLLATERAL_CREATE, covenant_create_undecoded),
    ("covenant 1900 collateral", C, BAD_SHARED_COLLATERAL_CREATE, covenant_create_not_collateral),
];

/// The literal reject sites EV1-3 lands that no input reaches, with the reason.
const UNREACHABLE: &[(TxValidationResult, &str, &str)] = &[
    (
        C,
        BAD_PROTX_TYPE,
        "the undefined-type branch of ProReg and ProUpServ: `validated_payload` \
         (`IsTriviallyValid`) rejects an undefined type first",
    ),
    (
        S,
        BAD_PROTX_NETINFO_ENTRY,
        "ProReg 1306 and ProUpServ 1453: `CheckNetInfo` (in `CheckProviderNetworkFields`) rejects \
         an entry that is neither an address nor a domain first",
    ),
    (
        S,
        BAD_PROUPSHARE_PAYEE_DEST,
        "ProUpShare 1652: `IsTriviallyValid` admits only P2PKH and P2SH rewards, which always have \
         a destination",
    ),
];

#[test]
fn reject_vectors() {
    let failed: Vec<String> = CASES
        .iter()
        .filter_map(|(name, class, reason, run)| {
            let got = run();
            (!is(&got, *class, reason))
                .then(|| format!("{name}: {got:?}, expected {class:?} {reason}"))
        })
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
fn dispatch_accepts_a_transaction_without_a_payload_field() {
    assert_eq!(dispatch_no_payload_field(), Ok(()));
}

/// Census closure (§6.1) over EV1-3's sites: every row is a census row with its class; each
/// literal reject site in the check modules has its own vector or `UNREACHABLE` row (a second
/// site reusing a reason needs a second vector); each reason the signature wrappers write is
/// reached.
#[test]
fn census_closure() {
    let census = |reason: &str, class| CLASSES.contains(&(reason, class));
    let mut rows: BTreeMap<&str, usize> = BTreeMap::new();
    for (name, class, reason, _) in CASES {
        assert!(census(reason, *class), "{name}: {reason} {class:?} not in the census");
        *rows.entry(reason).or_default() += 1;
    }
    for (class, reason, why) in UNREACHABLE {
        assert!(census(reason, *class), "{reason}: {why}");
        assert!(!rows.contains_key(reason), "{reason} is listed unreachable but has a vector");
        rows.insert(reason, 1);
    }
    let checks = [
        include_str!("mod.rs"),
        include_str!("provider.rs"),
        include_str!("shared.rs"),
        include_str!("covenant.rs"),
    ];
    let short: Vec<String> = literal_reasons(&checks)
        .into_iter()
        .filter_map(|(name, sites)| {
            let reason =
                reason_of(&name).unwrap_or_else(|| panic!("{name} is not a reason constant"));
            let covered = rows.get(reason).copied().unwrap_or_default();
            (covered < sites).then(|| format!("{name}: {sites} sites, {covered} vectors"))
        })
        .collect();
    assert!(short.is_empty(), "{short:#?}");
    for name in literal_reasons(&[include_str!("sig.rs")]).into_keys() {
        let reason = reason_of(&name).unwrap_or_else(|| panic!("{name} is not a reason constant"));
        assert!(rows.contains_key(reason), "{name} ({reason}) has no vector");
    }
}

/// The reason constants `sources` name outside `use` lines, comments and tests, with their
/// number of sites.
fn literal_reasons(sources: &[&str]) -> BTreeMap<String, usize> {
    let mut sites = BTreeMap::new();
    for source in sources {
        let code = source.split_once("#[cfg(test)]").map_or(*source, |(code, _)| code);
        let lines = code.lines().map(str::trim_start);
        for line in lines.filter(|l| !l.starts_with("use ") && !l.starts_with("//")) {
            let words = line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'));
            for word in words.filter(|w| w.starts_with("BAD_") || w.starts_with("FAILED_")) {
                *sites.entry(word.to_owned()).or_default() += 1;
            }
        }
    }
    sites
}

/// The value of the reason constant `name`, read from `reasons.rs`.
fn reason_of(name: &str) -> Option<&'static str> {
    let prefix = format!("pub const {name}: &str = \"");
    include_str!("reasons.rs")
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str())?.strip_suffix("\";"))
}

mod updates;
