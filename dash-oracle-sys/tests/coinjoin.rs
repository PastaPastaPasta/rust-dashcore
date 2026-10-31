//! The CoinJoin rules (ABI 26, O2a-CJ1): the shim's refusals (`oracle.h` "CoinJoin rules") and
//! its seams. Core's verdicts on generated inputs, against dash-coinjoin, are node-l1's
//! `l1_coinjoin`; the values asserted here are Core's constants (`coinjoin/common.h`
//! `vecStandardDenominations`, `GetCollateralAmount`), which the oracle returns.

use dash_oracle_sys::{
    Chain, Checkpoints, CjEntryKind, CjMessage, CjRule, CjView, CoinsView, Context, ContextCoin,
    DEPLOYMENT_V24, ErrorKind, MnList, Oracle, Pin, ThresholdState,
};

const SMALLEST: i64 = 100_001;
const COLLATERAL: i64 = SMALLEST / 10;

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

fn regtest() -> Chain {
    Chain::new("regtest", &[])
}

fn compact(out: &mut Vec<u8>, n: usize) {
    assert!(n < 0xfd);
    out.push(n as u8);
}

fn p2pkh(tag: u8) -> Vec<u8> {
    [&[0x76, 0xa9, 0x14][..], &[tag; 20], &[0x88, 0xac]].concat()
}

/// A serialized `std::vector<CTxIn>` of `prevouts` (empty scripts, final sequences).
fn vin(prevouts: &[([u8; 32], u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    compact(&mut out, prevouts.len());
    for (txid, n) in prevouts {
        out.extend(txid);
        out.extend(n.to_le_bytes());
        out.push(0);
        out.extend(u32::MAX.to_le_bytes());
    }
    out
}

/// A serialized `std::vector<CTxOut>`.
fn vout(outputs: &[(i64, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    compact(&mut out, outputs.len());
    for (value, script) in outputs {
        out.extend(value.to_le_bytes());
        compact(&mut out, script.len());
        out.extend(script);
    }
    out
}

/// A serialized transaction (version 2, or 3 with `tx_type` and an empty payload).
fn tx(
    tx_type: u16,
    prevouts: &[([u8; 32], u32)],
    outputs: &[(i64, Vec<u8>)],
    lock_time: u32,
) -> Vec<u8> {
    let version: u16 = if tx_type == 0 {
        2
    } else {
        3
    };
    let mut out = (u32::from(version) | u32::from(tx_type) << 16).to_le_bytes().to_vec();
    out.extend(vin(prevouts));
    out.extend(vout(outputs));
    out.extend(lock_time.to_le_bytes());
    if tx_type != 0 {
        out.push(0);
    }
    out
}

fn coin(txid: u8, value: i64, script: Vec<u8>) -> ContextCoin {
    ContextCoin {
        txid: [txid; 32],
        vout: 0,
        height: 1,
        coinbase: false,
        amount: value,
        script_pubkey: script,
    }
}

#[test]
fn rules_and_their_domain() {
    let o = oracle();
    let rule = |r: CjRule, args: &[i64]| o.cj_rule(r, args);
    let refused = |r: CjRule, args: &[i64]| {
        assert_eq!(rule(r, args).unwrap_err().kind, ErrorKind::Argument, "{r:?} {args:?}");
    };
    assert_eq!(rule(CjRule::AmountToDenomination, &[SMALLEST]).unwrap(), 1 << 4);
    assert_eq!(rule(CjRule::DenominationToAmount, &[1 << 4]).unwrap(), SMALLEST);
    assert_eq!(rule(CjRule::IsCollateralAmount, &[COLLATERAL]).unwrap(), 1);
    assert_eq!(rule(CjRule::AdjacentDenominations, &[1 << 3, 1 << 4]).unwrap(), 1);
    refused(CjRule::AmountToDenomination, &[]);
    refused(CjRule::AdjacentDenominations, &[1]);
    refused(CjRule::DenominationToAmount, &[i64::from(i32::MAX) + 1]);
    refused(CjRule::ShouldPromote, &[-1, 0, 0, 0]);
    refused(CjRule::ShouldDemote, &[0, 0, 0, i64::from(i32::MAX / 2) + 1]);
    rule(CjRule::ShouldDemote, &[0, 0, 0, i64::from(i32::MAX / 2)]).unwrap();
    refused(CjRule::FinalTxComposition, &[-1, 0, 0, 0]);
    // Core's size_t products wrap: 10 x ceil(2^64 / 10) larger outputs need 4 session inputs.
    let wraps = (u64::MAX / 10 + 1) as i64;
    assert_eq!(rule(CjRule::FinalTxComposition, &[i64::MAX, i64::MAX, 0, wraps]).unwrap(), 1);
    assert_eq!(rule(CjRule::FinalTxComposition, &[3, i64::MAX, 0, wraps]).unwrap(), 0);
}

#[test]
fn entries() {
    let o = oracle();
    let ten: Vec<([u8; 32], u32)> = (0..10).map(|i| ([i; 32], 0)).collect();
    let promotion = o
        .cj_entry(CjEntryKind::Promotion, &vin(&ten), &vout(&[(10 * SMALLEST, p2pkh(1))]), 1 << 4)
        .unwrap();
    // 10 x 100001 = 1000010, the next larger denomination.
    assert!(promotion.accepted, "{promotion:?}");
    assert_eq!(promotion.message, 19, "MSG_NOERR");
    let size = o.cj_entry(CjEntryKind::Demotion, &vin(&ten), &vout(&[]), 1 << 4).unwrap();
    assert!(!size.accepted);
    assert_eq!(size.message, 22, "ERR_SIZE_MISMATCH");
    let e = o.cj_entry(CjEntryKind::Demotion, &vin(&ten)[..5], &vout(&[]), 1 << 4).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Decode);
    let mut trailing = vin(&ten);
    trailing.push(0);
    let e = o.cj_entry(CjEntryKind::Demotion, &trailing, &vout(&[]), 1 << 4).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Trailing);
}

/// A view the shim refuses is `ORACLE_E_ARG`, as the header lists.
#[test]
fn view_refusals() {
    let o = oracle();
    let run =
        |view: &CjView| o.cj_in_outs(&regtest(), view, &vin(&[]), &vout(&[]), 1 << 4, false, false);
    let refused = |view: CjView, why: &str| {
        assert_eq!(run(&view).unwrap_err().kind, ErrorKind::Argument, "{why}");
    };
    let spend = |txid: u8, out_tag: u8| tx(0, &[([txid; 32], 0)], &[(SMALLEST, p2pkh(out_tag))], 0);
    let ok = CjView {
        coins: vec![coin(1, SMALLEST, p2pkh(1))],
        mempool: vec![spend(2, 2), spend(3, 3)],
        locked: vec![[4; 32]],
    };
    assert!(run(&ok).unwrap().accepted, "an empty entry is a valid standard one");
    // Session denomination 0, which no session has: Core's fee sums would add unchecked amounts.
    let at_zero = o.cj_in_outs(&regtest(), &ok, &vin(&[]), &vout(&[]), 0, false, false);
    assert_eq!(at_zero.unwrap_err().kind, ErrorKind::Argument, "session denomination 0");
    let with = |f: &dyn Fn(&mut CjView)| {
        let mut v = ok.clone();
        f(&mut v);
        v
    };
    refused(with(&|v| v.coins.push(coin(1, SMALLEST, p2pkh(9)))), "a repeated coin");
    refused(with(&|v| v.coins.push(coin(5, -1, p2pkh(9)))), "a spent coin");
    refused(with(&|v| v.coins.push(coin(5, 1, vec![0x6a]))), "an unspendable coin");
    let at_mempool_height = ContextCoin {
        height: i32::MAX as u32,
        ..coin(5, 1, p2pkh(9))
    };
    refused(with(&|v| v.coins.push(at_mempool_height.clone())), "a coin at MEMPOOL_HEIGHT");
    refused(with(&|v| v.mempool.push(spend(2, 2))), "a repeated txid");
    refused(with(&|v| v.mempool.push(spend(2, 7))), "a double spend");
    let coinbase = tx(0, &[([0; 32], u32::MAX)], &[(1, p2pkh(8))], 0);
    refused(with(&|v| v.mempool.push(coinbase.clone())), "a coinbase");
    let special = tx(8, &[([6; 32], 0)], &[(1, p2pkh(8))], 0);
    refused(with(&|v| v.mempool.push(special.clone())), "a special transaction");
}

/// The mempool and lock seams: a mempool coin is usable once its txid is locked.
#[test]
fn in_outs_seams() {
    let o = oracle();
    let parent = tx(0, &[([1; 32], 0)], &[(SMALLEST, p2pkh(1))], 0);
    let parent_txid = {
        use bitcoin_hashes::{Hash, sha256d};
        sha256d::Hash::hash(&parent).to_byte_array()
    };
    let mut view = CjView {
        coins: vec![],
        mempool: vec![parent],
        locked: vec![],
    };
    let entry = |view: &CjView| {
        o.cj_in_outs(
            &regtest(),
            view,
            &vin(&[(parent_txid, 0)]),
            &vout(&[(SMALLEST, p2pkh(2))]),
            1 << 4,
            false,
            false,
        )
        .unwrap()
    };
    let unlocked = entry(&view);
    assert!(!unlocked.accepted);
    assert_eq!((unlocked.message, unlocked.consume_collateral), (17, false), "ERR_MISSING_TX");
    view.locked.push(parent_txid);
    let locked = entry(&view);
    assert!(locked.accepted, "{locked:?}");
    assert_eq!(locked.counts, [1, 1]);
}

/// Collateral: the mempool answer is the caller's, asked once, and sums that would overflow
/// where Core adds them are refused.
#[test]
fn collateral() {
    let o = oracle();
    let funded = CjView {
        coins: vec![coin(1, 2 * COLLATERAL, p2pkh(1))],
        ..CjView::default()
    };
    let spend = |outputs: &[(i64, Vec<u8>)]| tx(0, &[([1; 32], 0)], outputs, 0);
    let valid = spend(&[(COLLATERAL, p2pkh(2))]);
    let accepted = o.cj_collateral(&regtest(), &funded, &valid, Some(COLLATERAL)).unwrap();
    assert!(accepted.valid);
    assert_eq!((accepted.test_accept_calls, accepted.submit_calls), (1, 0));
    let refused = o.cj_collateral(&regtest(), &funded, &valid, None).unwrap();
    assert!(!refused.valid);
    assert_eq!(refused.test_accept_calls, 1);
    // A fee above DEFAULT_MAX_RAW_TX_FEE (0.1 DASH) fails ATMPIfSaneFee.
    let rich = o.cj_collateral(&regtest(), &funded, &valid, Some(10_000_001)).unwrap();
    assert!(!rich.valid);

    let overflow = |view: &CjView, outputs: &[(i64, Vec<u8>)]| {
        o.cj_collateral(&regtest(), view, &spend(outputs), None)
    };
    let e = overflow(&funded, &[(i64::MAX, p2pkh(2)), (1, p2pkh(3))]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "the output sum");
    // Core returns at the first non-P2PKH output, after adding it: nothing overflows.
    let early = overflow(&funded, &[(i64::MAX, vec![0x51]), (1, p2pkh(3))]).unwrap();
    assert!(!early.valid);
    assert_eq!(early.test_accept_calls, 0);
    let negative = CjView {
        coins: vec![coin(1, i64::MIN, p2pkh(1))],
        ..CjView::default()
    };
    let e = overflow(&negative, &[(1, p2pkh(2))]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "input minus output");
    // A difference of INT64_MIN: the log line's nValueOut - nValueIn would overflow.
    let e = overflow(&negative, &[(0, p2pkh(2))]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "a difference of INT64_MIN");
    let near = CjView {
        coins: vec![coin(1, i64::MIN + 1, p2pkh(1))],
        ..CjView::default()
    };
    assert!(!overflow(&near, &[(0, p2pkh(2))]).unwrap().valid);

    // The input sum, refused when Core reaches it and not after an input it finds no coin for.
    let huge = CjView {
        coins: vec![coin(1, i64::MAX, p2pkh(1)), coin(2, i64::MAX, p2pkh(1))],
        ..CjView::default()
    };
    let spend_of = |prevouts: &[u8]| {
        let prevouts: Vec<([u8; 32], u32)> = prevouts.iter().map(|&t| ([t; 32], 0)).collect();
        tx(0, &prevouts, &[(1, p2pkh(2))], 0)
    };
    let e = o.cj_collateral(&regtest(), &huge, &spend_of(&[1, 2]), None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "the input sum");
    let missing_first = o.cj_collateral(&regtest(), &huge, &spend_of(&[3, 1, 2]), None).unwrap();
    assert!(!missing_first.valid);
    assert_eq!(missing_first.test_accept_calls, 0);
    // A mempool coin first: unusable while unlocked (Core stops there), added once locked.
    let parent = tx(0, &[([9; 32], 0)], &[(2 * COLLATERAL, p2pkh(1))], 0);
    let parent_txid = {
        use bitcoin_hashes::{Hash, sha256d};
        sha256d::Hash::hash(&parent).to_byte_array()
    };
    let mut pending = CjView {
        mempool: vec![parent],
        ..huge.clone()
    };
    let prevouts = [(parent_txid, 0), ([1; 32], 0), ([2; 32], 0)];
    let spend_pending = tx(0, &prevouts, &[(1, p2pkh(2))], 0);
    let unlocked = o.cj_collateral(&regtest(), &pending, &spend_pending, None).unwrap();
    assert!(!unlocked.valid);
    pending.locked.push(parent_txid);
    let e = o.cj_collateral(&regtest(), &pending, &spend_pending, None).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "the input sum after a locked mempool coin");
}

/// A signed queue: its hashes, its signature under the operator key in either scheme's
/// encoding, the time bound and decode failures.
#[test]
fn messages() {
    use dash_pkc::bls::core::BlsPkValue;
    use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};

    let o = oracle();
    let sk = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).unwrap();
    let mut dsq = (1i32 << 4).to_le_bytes().to_vec();
    dsq.extend([0x11; 32]);
    dsq.extend(1_760_000_000i64.to_le_bytes());
    dsq.push(1);
    let unsigned = o.cj_message_hash(CjMessage::Dsq, &[&dsq[..], &[0]].concat()).unwrap();
    assert!(unsigned.hash.is_some());
    let sig = sk.sign_with(&unsigned.sig_hash, BlsSigId::Basic).to_bytes();
    dsq.push(96);
    dsq.extend(sig);
    let hashes = o.cj_message_hash(CjMessage::Dsq, &dsq).unwrap();
    assert_eq!(hashes.sig_hash, unsigned.sig_hash, "the signature is not signed");
    assert_ne!(hashes.hash, unsigned.hash, "the inventory hash covers it");

    let pk = BlsPkValue::from_canonical(&sk.public_key().to_bytes(), BlsSchemeKind::Basic).unwrap();
    for scheme in [BlsSchemeKind::Basic, BlsSchemeKind::Legacy] {
        let legacy = scheme == BlsSchemeKind::Legacy;
        assert!(o.cj_check_signature(CjMessage::Dsq, &dsq, &pk.to_bytes(scheme), legacy).unwrap());
    }
    assert!(!o.cj_check_signature(CjMessage::Dsq, &dsq, &[0; 48], false).unwrap());
    let e = o.cj_check_signature(CjMessage::Dstx, &dsq, &pk.to_bytes(BlsSchemeKind::Basic), false);
    assert!(matches!(e.unwrap_err().kind, ErrorKind::Decode | ErrorKind::Trailing));

    for (now, out) in [(1_760_000_030, false), (1_760_000_031, true), (-1, true)] {
        assert_eq!(o.cj_queue_time_out_of_bounds(&dsq, now).unwrap(), out, "{now}");
    }
}

/// The structure check reads V24 from the context: on regtest a balanced broadcast of three
/// is valid either way, a ten-to-one only post-V24 (and possibly valid before it).
#[test]
fn dstx_structure() {
    let o = oracle();
    let dstx = |ins: u8, outs: u8| {
        let prevouts: Vec<([u8; 32], u32)> = (0..ins).map(|i| ([i; 32], 0)).collect();
        let outputs: Vec<(i64, Vec<u8>)> = (0..outs).map(|i| (SMALLEST, p2pkh(i))).collect();
        let mut m = tx(0, &prevouts, &outputs, 0);
        m.extend([0x11; 32]);
        m.push(0);
        m.extend(1_760_000_000i64.to_le_bytes());
        m
    };
    let ctx = |v24: ThresholdState| {
        let mut header = [0u8; 80];
        header[0] = 4;
        Context::new(
            100,
            vec![header],
            false,
            vec![(DEPLOYMENT_V24, v24)],
            0,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            MnList::Absent,
            vec![],
        )
    };
    let check = |m: &[u8], v24| o.cj_dstx_structure(&regtest(), m, &ctx(v24)).unwrap();
    for v24 in [ThresholdState::Defined, ThresholdState::Active] {
        assert!(check(&dstx(3, 3), v24).valid);
    }
    let pre = check(&dstx(10, 1), ThresholdState::Defined);
    assert!(!pre.valid && pre.possibly_valid_post_v24, "{pre:?}");
    let post = check(&dstx(10, 1), ThresholdState::Active);
    assert!(post.valid && !post.possibly_valid_post_v24, "{post:?}");
    // No masternode named: invalid whatever the rules.
    let mut anonymous = dstx(3, 3);
    let at = anonymous.len() - 8 - 1 - 32;
    anonymous[at..at + 32].fill(0);
    assert!(!check(&anonymous, ThresholdState::Active).valid);
}
