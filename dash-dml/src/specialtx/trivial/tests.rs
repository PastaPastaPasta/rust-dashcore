//! L0 vectors of the trivial checks (EV1 §6.1): every reachable reject row of
//! `providertx.cpp:20-164, 265-586` with Core's class and reason, on payloads mutated from Core's
//! `trivially_valid.json`, plus `multipayout_list_validation` (`evo_trivialvalidation.cpp:153-206`).
//! The census closure test (`reasons::tests`) checks that each row here is a census row.

use dash_consensus::primitives::{Tx, decode_tx_exact};
use dash_consensus::validation::TxValidationResult;
use dash_core_vectors::cases::{BlsScheme, load_trivial};
use dash_pkc::bls::{BlsLazyPk, BlsSchemeKind, BlsSchemeSource};
use dash_primitives::{CompactSig, NetInfo, TxHash};

use super::*;
use crate::specialtx::network::tests::{empty_mn, ext, ipv4_entry};

const C: TxValidationResult = TxValidationResult::Consensus;
const S: TxValidationResult = TxValidationResult::BadSpecial;

/// The `providertx.cpp` rows below reach, with Core's class and reason.
pub(crate) const REACHED: &[(u32, TxValidationResult, &str)] = &[
    (29, S, BAD_PROTX_PAYOUTS_COUNT),
    (36, S, BAD_PROTX_PAYOUT_REWARD),
    (41, S, BAD_PROTX_PAYEE),
    (44, S, BAD_PROTX_PAYEE_DUP),
    (53, S, BAD_PROTX_PAYEE_REUSE),
    (58, S, BAD_PROTX_PAYOUT_REWARD_SUM),
    (67, S, BAD_PROTX_SHARES_COUNT),
    (70, S, BAD_PROTX_SHARES_EARLY_PERIOD),
    (80, S, BAD_PROTX_SHARES_AMOUNT),
    (86, S, BAD_PROTX_SHARES_KEY_NULL),
    (89, S, BAD_PROTX_SHARES_DUP_KEY),
    (92, S, BAD_PROTX_SHARES_DUP_REFUND),
    (101, S, BAD_PROTX_SHARES_PAYEE),
    (108, S, BAD_PROTX_SHARES_PAYEE_REUSE),
    (112, S, BAD_PROTX_SHARES_PAYEE_REUSE),
    (118, S, BAD_PROTX_SHARES_AMOUNT_SUM),
    (121, S, BAD_PROTX_SHARES_PENALTY),
    (127, S, BAD_PROTX_SHARES_PENALTY),
    (152, S, BAD_PROTX_COLLATERAL_REUSE),
    (159, S, BAD_PROTX_PAYEE_REUSE),
    (268, C, BAD_PROTX_VERSION),
    (271, C, BAD_PROTX_EVO_VERSION),
    (274, C, BAD_PROTX_TYPE),
    (277, C, BAD_PROTX_MODE),
    (283, S, BAD_PROTX_SHARES_EXTERNAL),
    (287, S, BAD_PROTX_SHARES_OWNER_KEY),
    (290, S, BAD_PROTX_SHARES_PAYOUTS),
    (294, S, BAD_PROTX_SHARES_EMPTY_FIELDS),
    (297, S, BAD_PROTX_KEY_NULL),
    (301, S, BAD_PROTX_KEY_NULL),
    (316, C, BAD_PROTX_NETINFO_VERSION),
    (324, S, BAD_PROTX_OPERATOR_REWARD),
    (410, C, BAD_PROTX_VERSION),
    (413, C, BAD_PROTX_EVO_VERSION),
    (416, C, BAD_PROTX_TYPE),
    (419, C, BAD_PROTX_NETINFO_VERSION),
    (422, S, BAD_PROTX_NETINFO_EMPTY),
    (452, C, BAD_PROTX_VERSION),
    (455, C, BAD_PROTX_MODE),
    (459, S, BAD_PROTX_KEY_NULL),
    (479, C, BAD_PROTX_VERSION),
    (485, C, BAD_PROTX_REASON),
    (528, C, BAD_PRODIS_VERSION),
    (531, S, BAD_PRODIS_SIG_COUNT),
    (545, C, BAD_PROUPSHARE_VERSION),
    (548, S, BAD_PROUPSHARE_SIG_SIZE),
    (551, S, BAD_PROUPSHARE_PAYEE),
    (567, C, BAD_PROUPSHAREDREG_VERSION),
    (570, S, BAD_PROTX_KEY_NULL),
    (576, S, BAD_PROUPSHAREDREG_SIG_COUNT),
];

fn expect(line: u32) -> (TxValidationResult, String) {
    let &(_, class, reason) = REACHED.iter().find(|r| r.0 == line).expect("a REACHED row");
    (class, reason.to_owned())
}

fn verdict(r: Result<(), TxValidationError>) -> Option<(TxValidationResult, String)> {
    r.err().map(|e| (e.result(), e.reject_reason().to_owned()))
}

/// The payload of the first `trivially_valid.json` row of `tx_type` and `scheme`.
fn core_payload(tx_type: &str, scheme: BlsScheme) -> SpecialPayload {
    let file = "trivially_valid.json";
    let text = dash_core_vectors::read_core_text(&format!("src/test/data/{file}"));
    let case = load_trivial(file, &text)
        .expect(file)
        .into_iter()
        .find(|c| c.tx_type == tx_type && c.scheme == scheme)
        .expect("a valid row");
    let tx: Tx = decode_tx_exact(&case.tx).expect("decodes").into_value();
    match tx.payload() {
        Some(Ok(d)) if d.is_complete() => d.payload.clone(),
        other => panic!("{tx_type}: {other:?}"),
    }
}

pub(crate) fn pro_reg(scheme: BlsScheme) -> ProRegTx {
    match core_payload("proregtx", scheme) {
        SpecialPayload::ProviderRegister(p) => p,
        p => panic!("{p:?}"),
    }
}

pub(crate) fn pro_up_serv(scheme: BlsScheme) -> ProUpServTx {
    match core_payload("proupservtx", scheme) {
        SpecialPayload::ProviderUpdateService(p) => p,
        p => panic!("{p:?}"),
    }
}

fn pro_up_reg(scheme: BlsScheme) -> ProUpRegTx {
    match core_payload("proupregtx", scheme) {
        SpecialPayload::ProviderUpdateRegistrar(p) => p,
        p => panic!("{p:?}"),
    }
}

fn pro_up_rev(scheme: BlsScheme) -> ProUpRevTx {
    match core_payload("prouprevtx", scheme) {
        SpecialPayload::ProviderUpdateRevoke(p) => p,
        p => panic!("{p:?}"),
    }
}

fn key(n: u8) -> KeyId {
    KeyId::from([n; 20])
}

fn p2pkh(hash: [u8; 20]) -> Vec<u8> {
    let mut s = vec![OP_DUP, OP_HASH160, 20];
    s.extend(hash);
    s.extend([OP_EQUALVERIFY, OP_CHECKSIG]);
    s
}

fn p2sh(hash: [u8; 20]) -> Vec<u8> {
    let mut s = vec![OP_HASH160, 20];
    s.extend(hash);
    s.push(OP_EQUAL);
    s
}

fn payout(hash: u8, reward: u16) -> PayoutShare {
    PayoutShare {
        script_payout: p2pkh([hash; 20]).into(),
        reward,
    }
}

fn share(n: u8, amount: i64) -> CollateralShare {
    CollateralShare {
        amount,
        script_refund: p2pkh([0x40 + n; 20]).into(),
        script_reward: vec![].into(),
        key_id_owner: key(0x20 + n),
    }
}

/// A Regular ExtNetInfo with one Core P2P entry.
fn ext_regular() -> NetInfo {
    ext(&[(0, vec![ipv4_entry([1, 1, 1, 1], 9999)])])
}

/// A valid version-3 (ExtAddr) Regular registration with a payout list, from Core's basic one.
fn pro_reg_v3() -> ProRegTx {
    let mut p = pro_reg(BlsScheme::Basic);
    p.version = 3;
    p.mn_type = PayloadMnType::Regular;
    p.net_info = ext_regular();
    p.payouts = vec![payout(0x31, 6000), payout(0x32, 4000)];
    p.script_payout = vec![].into();
    p
}

/// A valid shared registration: internal collateral, no owner key, two shares of a Regular
/// collateral.
fn pro_reg_shared() -> ProRegTx {
    let mut p = pro_reg_v3();
    p.collateral_hash = Default::default();
    p.key_id_owner = KeyId::default();
    p.payouts.clear();
    p.shares = vec![share(1, 600 * COIN), share(2, 400 * COIN)];
    p.early_period_blocks = 100;
    p.early_penalty = 10 * COIN;
    p
}

/// A valid version-3 (ExtAddr) payload of each of types 1 to 4.
pub(crate) fn ext_addr_payloads() -> (ProRegTx, ProUpServTx, ProUpRegTx, ProUpRevTx) {
    let mut serv = pro_up_serv(BlsScheme::Basic);
    (serv.version, serv.net_info) = (3, ext_regular());
    let mut up_reg = pro_up_reg(BlsScheme::Basic);
    (up_reg.version, up_reg.script_payout, up_reg.payouts) =
        (3, vec![].into(), vec![payout(0x31, 10_000)]);
    let mut rev = pro_up_rev(BlsScheme::Basic);
    rev.version = 3;
    (pro_reg_v3(), serv, up_reg, rev)
}

fn valid_basic_key() -> BlsLazyPk {
    pro_up_reg(BlsScheme::Basic).pub_key_operator
}

pub(crate) fn pro_dis() -> ProDisTx {
    ProDisTx {
        version: 1,
        pro_tx_hash: Default::default(),
        actor_index: 0,
        sigs: vec![CompactSig::from_bytes([1; 65])],
    }
}

fn pro_up_share() -> ProUpShareTx {
    ProUpShareTx {
        version: 1,
        pro_tx_hash: Default::default(),
        share_index: 0,
        script_reward: p2pkh([5; 20]).into(),
        inputs_hash: Default::default(),
        vch_sig: vec![1; 65],
    }
}

fn pro_up_shared_reg() -> ProUpSharedRegTx {
    ProUpSharedRegTx {
        version: 1,
        pro_tx_hash: Default::default(),
        pub_key_operator: valid_basic_key(),
        key_id_voting: key(9),
        inputs_hash: Default::default(),
        sigs: vec![CompactSig::from_bytes([1; 65]); 2],
    }
}

#[test]
fn bases_are_trivially_valid() {
    for scheme in [BlsScheme::Legacy, BlsScheme::Basic] {
        assert_eq!(pro_reg_trivially_valid(&pro_reg(scheme)), Ok(()));
        assert_eq!(pro_up_serv_trivially_valid(&pro_up_serv(scheme)), Ok(()));
        assert_eq!(pro_up_reg_trivially_valid(&pro_up_reg(scheme)), Ok(()));
        assert_eq!(pro_up_rev_trivially_valid(&pro_up_rev(scheme)), Ok(()));
    }
    assert_eq!(pro_reg_trivially_valid(&pro_reg_v3()), Ok(()));
    assert_eq!(pro_reg_trivially_valid(&pro_reg_shared()), Ok(()));
    let (reg, serv, up_reg, rev) = ext_addr_payloads();
    assert_eq!(pro_reg_trivially_valid(&reg), Ok(()));
    assert_eq!(pro_up_serv_trivially_valid(&serv), Ok(()));
    assert_eq!(pro_up_reg_trivially_valid(&up_reg), Ok(()));
    assert_eq!(pro_up_rev_trivially_valid(&rev), Ok(()));
    assert_eq!(pro_dis_trivially_valid(&pro_dis()), Ok(()));
    assert_eq!(pro_up_share_trivially_valid(&pro_up_share()), Ok(()));
    assert_eq!(pro_up_shared_reg_trivially_valid(&pro_up_shared_reg()), Ok(()));
}

/// One vector per reachable row, each a single fault on a valid base.
#[test]
fn every_row_has_a_vector() {
    let legacy = BlsScheme::Legacy;
    let reg = |f: &dyn Fn(&mut ProRegTx)| {
        let mut p = pro_reg_v3();
        f(&mut p);
        verdict(pro_reg_trivially_valid(&p))
    };
    let shared = |f: &dyn Fn(&mut ProRegTx)| {
        let mut p = pro_reg_shared();
        f(&mut p);
        verdict(pro_reg_trivially_valid(&p))
    };
    let serv = |f: &dyn Fn(&mut ProUpServTx)| {
        let mut p = pro_up_serv(legacy);
        f(&mut p);
        verdict(pro_up_serv_trivially_valid(&p))
    };
    let up_reg = |f: &dyn Fn(&mut ProUpRegTx)| {
        let mut p = pro_up_reg(legacy);
        f(&mut p);
        verdict(pro_up_reg_trivially_valid(&p))
    };
    let rev = |f: &dyn Fn(&mut ProUpRevTx)| {
        let mut p = pro_up_rev(legacy);
        f(&mut p);
        verdict(pro_up_rev_trivially_valid(&p))
    };
    let dis = |f: &dyn Fn(&mut ProDisTx)| {
        let mut p = pro_dis();
        f(&mut p);
        verdict(pro_dis_trivially_valid(&p))
    };
    let up_share = |f: &dyn Fn(&mut ProUpShareTx)| {
        let mut p = pro_up_share();
        f(&mut p);
        verdict(pro_up_share_trivially_valid(&p))
    };
    let shared_reg = |f: &dyn Fn(&mut ProUpSharedRegTx)| {
        let mut p = pro_up_shared_reg();
        f(&mut p);
        verdict(pro_up_shared_reg_trivially_valid(&p))
    };
    let undecodable_key =
        BlsLazyPk::from_bytes(&[0xff; 48], BlsSchemeSource::Version(BlsSchemeKind::Basic));
    let cases: Vec<(u32, Option<(TxValidationResult, String)>)> = vec![
        (29, reg(&|p| p.payouts.clear())),
        (36, reg(&|p| p.payouts = vec![payout(0x31, 99), payout(0x32, 9901)])),
        (41, reg(&|p| p.payouts[0].script_payout = vec![0x6a].into())),
        (44, reg(&|p| p.payouts = vec![payout(0x31, 5000), payout(0x31, 5000)])),
        // Against every earlier payout, not only the one before (`std::set`).
        (
            44,
            reg(&|p| p.payouts = vec![payout(0x31, 3000), payout(0x32, 4000), payout(0x31, 3000)]),
        ),
        (53, reg(&|p| p.payouts[0].script_payout = p2pkh(*p.key_id_voting.as_bytes()).into())),
        (58, reg(&|p| p.payouts[1].reward = 3999)),
        (67, shared(&|p| p.shares.truncate(1))),
        (70, shared(&|p| p.early_period_blocks = MAX_EARLY_PERIOD_BLOCKS + 1)),
        (80, shared(&|p| p.shares[0].amount = MIN_SHARE_AMOUNT - 1)),
        (86, shared(&|p| p.shares[1].key_id_owner = KeyId::default())),
        (89, shared(&|p| p.shares[1].key_id_owner = p.shares[0].key_id_owner)),
        (92, shared(&|p| p.shares[1].script_refund = p.shares[0].script_refund.clone())),
        // Against every earlier share, not only the first.
        (
            92,
            shared(&|p| {
                p.shares = vec![share(1, 400 * COIN), share(2, 300 * COIN), share(3, 300 * COIN)];
                p.shares[2].script_refund = p.shares[1].script_refund.clone();
            }),
        ),
        (101, shared(&|p| p.shares[0].script_reward = vec![0x51].into())),
        (108, shared(&|p| p.shares[0].script_refund = p2pkh(*p.key_id_voting.as_bytes()).into())),
        // The reward script is checked as the refund script is.
        (108, shared(&|p| p.shares[0].script_reward = p2pkh(*p.key_id_voting.as_bytes()).into())),
        (
            112,
            // The owner of a later share.
            shared(&|p| {
                p.shares[0].script_reward = p2pkh(*p.shares[1].key_id_owner.as_bytes()).into()
            }),
        ),
        (118, shared(&|p| p.shares[1].amount = 399 * COIN)),
        (121, shared(&|p| p.early_penalty = 400 * COIN)),
        (
            127,
            shared(&|p| {
                p.early_period_blocks = 0;
                p.early_penalty = 1;
            }),
        ),
        (268, reg(&|p| p.version = 0)),
        (
            271,
            reg(&|p| {
                p.version = 1;
                p.mn_type = PayloadMnType::Evo;
            }),
        ),
        (274, reg(&|p| p.mn_type = PayloadMnType::from_base(2))),
        (277, reg(&|p| p.mode = 1)),
        (283, shared(&|p| p.collateral_hash = TxHash::from([1; 32]))),
        (287, shared(&|p| p.key_id_owner = key(3))),
        (290, shared(&|p| p.payouts = vec![payout(0x31, 10000)])),
        (294, reg(&|p| p.early_penalty = 1)),
        (297, reg(&|p| p.key_id_owner = KeyId::default())),
        (301, reg(&|p| p.key_id_voting = KeyId::default())),
        (301, reg(&|p| p.pub_key_operator = undecodable_key.clone())),
        (316, reg(&|p| p.net_info = empty_mn())),
        (324, reg(&|p| p.operator_reward = MAX_OPERATOR_REWARD + 1)),
        (410, serv(&|p| p.version = 4)),
        (
            413,
            serv(&|p| {
                p.version = 1;
                p.mn_type = PayloadMnType::Evo;
            }),
        ),
        (416, serv(&|p| p.mn_type = PayloadMnType::from_base(42))),
        (419, serv(&|p| p.net_info = ext_regular())),
        (422, serv(&|p| p.net_info = empty_mn())),
        (452, up_reg(&|p| p.version = 0)),
        (455, up_reg(&|p| p.mode = 1)),
        (459, up_reg(&|p| p.key_id_voting = KeyId::default())),
        (479, rev(&|p| p.version = 0)),
        (485, rev(&|p| p.reason = RevocationReason::Unknown(4))),
        (528, dis(&|p| p.version = 2)),
        (531, dis(&|p| p.sigs = vec![CompactSig::from_bytes([1; 65]); MAX_SHARES + 1])),
        (545, up_share(&|p| p.version = 0)),
        (548, up_share(&|p| p.vch_sig.pop().map(drop).unwrap_or(()))),
        (548, up_share(&|p| p.vch_sig.push(1))),
        (551, up_share(&|p| p.script_reward = vec![0x51].into())),
        (567, shared_reg(&|p| p.version = 2)),
        (570, shared_reg(&|p| p.pub_key_operator = BlsLazyPk::new(BlsSchemeKind::Basic))),
        (576, shared_reg(&|p| p.sigs.clear())),
    ];
    for (line, got) in &cases {
        assert_eq!(got.as_ref(), Some(&expect(*line)), "providertx.cpp:{line}");
    }
    for (line, _, _) in REACHED {
        if (152..=159).contains(line) {
            continue; // `is_payout_list_key_safe` below.
        }
        assert!(cases.iter().any(|(l, _)| l == line), "providertx.cpp:{line} has no vector");
    }
}

/// `providertx.cpp:304, 462, 573` (`bad-protx-operator-pubkey`) are unreachable from wire bytes:
/// Core reads the operator key under the payload version's scheme (`providertx.h:180, 346`; basic
/// for type 12, `:534`). A payload built in memory reaches them: a decoded key whose scheme is then
/// changed (Core's `SetLegacy` keeps the decoded object).
#[test]
fn operator_key_scheme_rows() {
    let mut p = pro_reg(BlsScheme::Basic);
    p.pub_key_operator.get();
    p.pub_key_operator.set_scheme(BlsSchemeKind::Legacy);
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), Some((S, BAD_PROTX_OPERATOR_PUBKEY.into())));
    let mut p = pro_up_reg(BlsScheme::Legacy);
    p.pub_key_operator.get();
    p.pub_key_operator.set_scheme(BlsSchemeKind::Basic);
    assert_eq!(
        verdict(pro_up_reg_trivially_valid(&p)),
        Some((S, BAD_PROTX_OPERATOR_PUBKEY.into()))
    );
    let mut p = pro_up_shared_reg();
    p.pub_key_operator.get();
    p.pub_key_operator.set_scheme(BlsSchemeKind::Legacy);
    assert_eq!(
        verdict(pro_up_shared_reg_trivially_valid(&p)),
        Some((S, BAD_PROTX_OPERATOR_PUBKEY.into()))
    );
}

/// `multipayout_list_validation` (`evo_trivialvalidation.cpp:153-206`).
#[test]
fn core_multipayout_list_validation() {
    let (owner, voting) = (key(1), key(2));
    let list = |v: &[PayoutShare]| {
        verdict(is_payout_list_trivially_valid(OwnerPayouts::List(v), &owner, &voting))
            .map(|(_, r)| r)
    };
    let s = |r: &str| Some(r.to_owned());
    assert_eq!(list(&[payout(0x31, 10000)]), None);
    assert_eq!(list(&[payout(0x31, 5000), payout(0x32, 3000), payout(0x33, 2000)]), None);
    assert_eq!(list(&[]), s(BAD_PROTX_PAYOUTS_COUNT));
    let too_many: Vec<PayoutShare> = (0..9).map(|i| payout(0x40 + i, 1000)).collect();
    assert_eq!(list(&too_many), s(BAD_PROTX_PAYOUTS_COUNT));
    assert_eq!(list(&[payout(0x31, 99), payout(0x32, 9901)]), s(BAD_PROTX_PAYOUT_REWARD));
    assert_eq!(list(&[payout(0x31, MAX_PAYOUT_REWARD + 1)]), s(BAD_PROTX_PAYOUT_REWARD));
    assert_eq!(list(&[payout(0x31, 5000), payout(0x32, 4999)]), s(BAD_PROTX_PAYOUT_REWARD_SUM));
    assert_eq!(list(&[payout(0x31, 5000), payout(0x31, 5000)]), s(BAD_PROTX_PAYEE_DUP));
    let op_return = PayoutShare {
        script_payout: vec![0x6a].into(),
        reward: 10000,
    };
    assert_eq!(list(&[op_return]), s(BAD_PROTX_PAYEE));
    assert_eq!(list(&[payout(1, 10000)]), s(BAD_PROTX_PAYEE_REUSE));
    assert_eq!(list(&[payout(2, 10000)]), s(BAD_PROTX_PAYEE_REUSE));
    // A null owner is not compared, so a payout to the null key passes.
    let null = KeyId::default();
    let with_null = |v: &[PayoutShare]| {
        verdict(is_payout_list_trivially_valid(OwnerPayouts::List(v), &null, &voting))
            .map(|(_, r)| r)
    };
    assert_eq!(with_null(&[payout(0, 10000)]), None);
    assert_eq!(with_null(&[payout(2, 10000)]), s(BAD_PROTX_PAYEE_REUSE));

    let payout_dest = Destination::PubKeyHash([0x31; 20]);
    let safe = |payouts: &[PayoutShare], dest: &Destination, check: bool| {
        verdict(is_payout_list_key_safe(OwnerPayouts::List(payouts), dest, &owner, &voting, check))
    };
    assert_eq!(
        safe(&[payout(0x31, 10000)], &payout_dest, true),
        Some((S, BAD_PROTX_PAYEE_REUSE.into()))
    );
    assert_eq!(safe(&[payout(0x31, 10000)], &payout_dest, false), None);
    let p2sh_collateral = PayoutShare {
        script_payout: p2sh([0x55; 20]).into(),
        reward: 10000,
    };
    let p2sh_dest = extract_destination(p2sh_collateral.script_payout.as_bytes()).unwrap();
    assert_eq!(safe(&[p2sh_collateral], &p2sh_dest, true), expect_some(159));
    // The collateral pays the owner or the voting key (`:150-153`).
    assert_eq!(safe(&[], &Destination::of_key(&owner), false), expect_some(152));
    assert_eq!(safe(&[], &Destination::of_key(&voting), false), expect_some(152));
}

fn expect_some(line: u32) -> Option<(TxValidationResult, String)> {
    Some(expect(line))
}

/// A legacy payout script is one payout at the whole reward (`LegacyPayoutAsList`).
#[test]
fn legacy_owner_payouts() {
    let script = p2pkh([7; 20]);
    let payouts = OwnerPayouts::new(2, &[], &script);
    assert_eq!(payouts.len(), 1);
    assert_eq!(payouts.iter().collect::<Vec<_>>(), [(&script[..], MAX_PAYOUT_REWARD)]);
    let list = [payout(1, 10000)];
    assert_eq!(OwnerPayouts::new(3, &list, &script).len(), 1);
    assert!(OwnerPayouts::new(3, &[], &script).is_empty());
}

/// Each share's two scripts run every check before the next share starts (D19): share 0's reward
/// script fails before share 1's refund script.
#[test]
fn share_loop_order() {
    let mut p = pro_reg_shared();
    p.shares[0].script_reward = vec![0x51].into();
    p.shares[1].amount = 1;
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), Some((S, BAD_PROTX_SHARES_PAYEE.into())));
    // The amount bound comes before the key checks of the same share.
    let mut p = pro_reg_shared();
    p.shares[1].amount = 1;
    p.shares[1].key_id_owner = KeyId::default();
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), expect_some(80));
}

/// rc.4 dropped the entry loop (CE4-03): an invalid entry is not a trivial reject, so a later
/// trivial check gives the reason.
#[test]
fn invalid_entry_is_not_a_trivial_reject() {
    let mut p = pro_reg_v3();
    // An entry of an unknown type decodes cleared.
    p.net_info = ext(&[(0, vec![vec![0x07]])]);
    p.operator_reward = MAX_OPERATOR_REWARD + 1;
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), expect_some(324));
}

/// `ExtractDestination` finds a key id only in pay-to-pubkey-hash and pay-to-pubkey scripts.
#[test]
fn destinations() {
    assert_eq!(extract_destination(&p2pkh([7; 20])), Some(Destination::PubKeyHash([7; 20])));
    let mut p2pk = vec![33, 2];
    p2pk.extend([1u8; 32]);
    p2pk.push(OP_CHECKSIG);
    assert_eq!(extract_destination(&p2pk), Some(Destination::PubKeyHash(hash160(&p2pk[1..34]))));
    // A 33-byte push whose header announces 65 bytes, or no size, is not a valid-size key.
    for header in [0, 4, 5, 6, 7] {
        p2pk[1] = header;
        assert_eq!(extract_destination(&p2pk), None, "33 bytes, header {header}");
    }
    assert_eq!(extract_destination(&p2sh([7; 20])), Some(Destination::ScriptHash([7; 20])));
    assert_eq!(extract_destination(&[0x6a]), None);
    assert!(is_valid_payout_script(&p2sh([7; 20])));
    assert!(!is_valid_payout_script(&p2pk));
    // Uncompressed (0x04) and hybrid (0x06, 0x07) keys are 65 bytes.
    for header in [4, 6, 7] {
        let mut p2pk = vec![65, header];
        p2pk.extend([1u8; 64]);
        p2pk.push(OP_CHECKSIG);
        let id = hash160(&p2pk[1..66]);
        assert_eq!(extract_destination(&p2pk), Some(Destination::PubKeyHash(id)), "{header}");
    }
    // A 65-byte push whose header announces 33 bytes, or no size.
    for header in [0, 2, 3, 5] {
        let mut p2pk = vec![65, header];
        p2pk.extend([1u8; 64]);
        p2pk.push(OP_CHECKSIG);
        assert_eq!(extract_destination(&p2pk), None, "65 bytes, header {header}");
    }
}

/// `GetMnType(nType).collat_amount` (`dmn_types.h:34, :39`) and `CCollateralShare::MIN_AMOUNT`
/// (`providertx.h:58`), in duffs.
#[test]
fn collateral_and_share_amounts() {
    assert_eq!(collateral_amount(MnType::Regular), 100_000_000_000);
    assert_eq!(collateral_amount(MnType::Evo), 400_000_000_000);
    // The smallest share: 100 DASH passes, a duff less is `bad-protx-shares-amount`.
    for (amount, ok) in [(100 * COIN, true), (100 * COIN - 1, false)] {
        let mut p = pro_reg_shared();
        p.shares = vec![share(1, 1000 * COIN - amount), share(2, amount)];
        p.early_penalty = 10 * COIN;
        let want = (!ok).then(|| (S, BAD_PROTX_SHARES_AMOUNT.to_string()));
        assert_eq!(verdict(pro_reg_trivially_valid(&p)), want, "{amount}");
    }
}

#[test]
fn voting_key_safety() {
    let voting = key(9);
    let mut shares = vec![share(1, 500 * COIN), share(2, 500 * COIN)];
    assert!(is_share_list_voting_key_safe(&shares, &voting));
    shares[1].script_reward = p2pkh(*voting.as_bytes()).into();
    assert!(!is_share_list_voting_key_safe(&shares, &voting));
    shares[1].script_reward = vec![].into();
    shares[0].script_refund = p2pkh(*voting.as_bytes()).into();
    assert!(!is_share_list_voting_key_safe(&shares, &voting));
}

/// Every bound of the trivial checks at its edge: the last value that passes and the first that
/// does not (the mutation pass found the inclusive ones unpinned).
#[test]
fn bounds_at_their_edges() {
    let ok = None;
    let special = |r: &str| Some((S, r.to_string()));
    let consensus = |r: &str| Some((C, r.to_string()));

    // `MAX_PAYOUTS` (8) payouts.
    for (n, want) in [(8u8, ok.clone()), (9, special(BAD_PROTX_PAYOUTS_COUNT))] {
        let mut p = pro_reg_v3();
        p.payouts = (0..n).map(|i| payout(0x31 + i, 10_000 / u16::from(n))).collect();
        p.payouts[0].reward += 10_000 % u16::from(n);
        assert_eq!(verdict(pro_reg_trivially_valid(&p)), want, "{n} payouts");
    }
    // `MAX_EARLY_PERIOD_BLOCKS`.
    for (blocks, want) in [(420_480, ok.clone()), (420_481, special(BAD_PROTX_SHARES_EARLY_PERIOD))]
    {
        let mut p = pro_reg_shared();
        p.early_period_blocks = blocks;
        assert_eq!(verdict(pro_reg_trivially_valid(&p)), want, "{blocks} blocks");
    }
    // A share of the whole collateral passes its own bound; the sum then fails.
    let mut p = pro_reg_shared();
    p.shares = vec![share(1, 1000 * COIN), share(2, 100 * COIN)];
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), special(BAD_PROTX_SHARES_AMOUNT_SUM));
    // The penalty: zero passes, below zero does not.
    for (penalty, want) in [(0, ok.clone()), (-1, special(BAD_PROTX_SHARES_PENALTY))] {
        let mut p = pro_reg_shared();
        p.early_penalty = penalty;
        assert_eq!(verdict(pro_reg_trivially_valid(&p)), want, "penalty {penalty}");
    }
    // An empty network info is not checked; a non-empty one is.
    let mut p = pro_reg_v3();
    p.net_info = ext(&[]);
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), ok);
    p.net_info =
        ext(&[(0, vec![ipv4_entry([1, 1, 1, 1], 9999)]), (1, vec![ipv4_entry([1, 1, 1, 1], 1)])]);
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), special(BAD_PROTX_NETINFO_BAD));
    // `MAX_OPERATOR_REWARD`.
    let mut p = pro_reg_v3();
    p.operator_reward = 10_000;
    assert_eq!(verdict(pro_reg_trivially_valid(&p)), ok);
    // Version `ExtAddr` (3) is the highest of ProUpRegTx and ProUpRevTx.
    let mut p = pro_up_reg(BlsScheme::Basic);
    (p.version, p.script_payout, p.payouts) = (3, vec![].into(), vec![payout(0x31, 10_000)]);
    assert_eq!(verdict(pro_up_reg_trivially_valid(&p)), ok);
    p.version = 4;
    assert_eq!(verdict(pro_up_reg_trivially_valid(&p)), consensus(BAD_PROTX_VERSION));
    let mut p = pro_up_rev(BlsScheme::Basic);
    p.version = 3;
    assert_eq!(verdict(pro_up_rev_trivially_valid(&p)), ok);
    p.version = 4;
    assert_eq!(verdict(pro_up_rev_trivially_valid(&p)), consensus(BAD_PROTX_VERSION));
    // `MAX_SHARES` (8) signatures, and version 1 alone for the shared payloads.
    let mut p = pro_dis();
    p.sigs = vec![p.sigs[0]; 8];
    assert_eq!(verdict(pro_dis_trivially_valid(&p)), ok);
    let mut p = pro_up_shared_reg();
    p.sigs = vec![p.sigs[0]; 8];
    assert_eq!(verdict(pro_up_shared_reg_trivially_valid(&p)), ok);
    p.sigs.push(p.sigs[0]);
    assert_eq!(
        verdict(pro_up_shared_reg_trivially_valid(&p)),
        special(BAD_PROUPSHAREDREG_SIG_COUNT)
    );
    let mut p = pro_up_share();
    p.version = 2;
    assert_eq!(verdict(pro_up_share_trivially_valid(&p)), consensus(BAD_PROUPSHARE_VERSION));
}
