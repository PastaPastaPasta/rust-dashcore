//! L0 and L1 for EV2b's validated-block target (D34/D40): `diff_dml_block`, Core's check loop,
//! `BuildNewListFromBlock` and `ProcessBlock` (`oracle_dml_block_effects`, ABI 26) against our
//! build and `process_block`, effects included.
//!
//! L0 replays O2a-EV's crafted regtest blocks whole and one transaction at a time, moves them
//! to a 576th height for the snapshot row, and rejects testnet's DIP3 enforcement block in
//! `ProcessBlock`; L1 runs the target over generated inputs and over `diff_dml_apply`'s public
//! seeds (its input format), and over checked sequences (D54) and their public seeds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use dash_consensus::primitives::decode_tx;
use dash_dml::specialtx::reasons::CLASSES;
use dash_dml::testkit::replay::testnet;
use dash_dml::unknown_pro_tx_version;
use dash_oracle_sys::{
    Chain, Checkpoints, Context, DmlEffects, DmlStage, MnList, Oracle, Pin, Verdict,
};
use dash_primitives::{
    CollateralSig, NetInfo, NetInfoV2, ProRegTx, ProTxVersion, ProUpRevTx, ProUpServTx,
    Transaction, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use node_l1::Outcome;
use node_l1::dml_apply::checked::{FLAG_CHECKED, FLAG_PROBE, Walk};
use node_l1::dml_apply::{Case, assemble, cases, generate, split_block, testnet_case, with_one_tx};
use node_l1::dml_block::{
    Detailed, SequenceBlock, compare_block, compare_dml_block, compare_dml_block_detailed,
    compare_sequence,
};
use node_l1::dml_checks::{COVENANT, CheckOutcome, DISPATCH, TABLES, missing_checks};

fn tally(counts: &mut BTreeMap<String, usize>, key: String) {
    *counts.entry(key).or_default() += 1;
}

/// A block's outcome, and Core's rejecting stage when it had one.
fn outcome_label(d: &Detailed) -> String {
    let stage = d.core.as_ref().map(|(stage, _)| format!(" {stage:?}"));
    let pre_dip3 = if d.pre_dip3 {
        " before DIP3"
    } else {
        ""
    };
    format!("{:?}{}{pre_dip3}", d.outcome, stage.unwrap_or_default())
}

#[test]
fn crafted_blocks_match_the_checked_path() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut outcomes = BTreeMap::new();
    let (mut with_resets, mut with_snapshots) = (0, 0);
    for case in cases() {
        let chain = case.chain();
        let block = &case.block;
        let (core, effects) =
            oracle.dml_block_effects(&chain, block, &case.ctx, false, false).unwrap();
        if core.verdict == Verdict::Accept {
            assert!(effects.diff_row.is_some(), "{}: an accepted block's diff row", case.name);
        }
        with_resets += usize::from(!effects.resets.is_empty());
        with_snapshots += usize::from(effects.snapshot_row.is_some());
        let mut blocks = vec![(String::from("whole"), block.clone())];
        for i in 1..usize::from(block[80]) {
            if let Some(single) = with_one_tx(block, i) {
                blocks.push((format!("tx {i}"), single));
            }
        }
        for (part, bytes) in blocks {
            let outcome = compare_dml_block(&oracle, &chain, &case.params, &case.ctx, &bytes)
                .unwrap_or_else(|d| panic!("{} ({part}): {d}", case.name));
            tally(&mut outcomes, format!("{outcome:?}"));
        }
    }
    eprintln!(
        "{outcomes:?}, {with_resets} whole blocks with platform-ban resets, {with_snapshots} with \
         snapshot rows"
    );
    assert!(outcomes.get("BothAccept").is_some_and(|&n| n >= 60), "{outcomes:?}");
    // Past Core's check loop, the build rejections are compared.
    assert!(outcomes.get("BothReject").is_some_and(|&n| n >= 5), "{outcomes:?}");
    // The crafted ProUpServTxs revive listed masternodes: Core's resets are seen and compared.
    assert!(with_resets > 0);
}

/// `ctx` and `block` moved up so the block is at `height`: the headers, the checkpoint height, the
/// coinbase's cbTx `nHeight` and the tip's list's height follow (Core's check loop compares the
/// last with the tip, `bad-cbtx-height`), and the block's commitment transactions are dropped
/// (their heights and quorum bases would no longer line up: `bad-qc-height`). The crafted
/// regtest blocks sit at 512–524, below the first 576th height, so this is how a case reaches
/// `ProcessBlock`'s snapshot row.
fn moved_to(ctx: &Context, block: &[u8], height: i32) -> (Context, Vec<u8>) {
    let delta = height - (ctx.first_height + ctx.headers.len() as i32);
    let mut ctx = ctx.clone();
    ctx.first_height += delta;
    if let Checkpoints::Enabled {
        best_header_height,
    } = &mut ctx.checkpoints
    {
        *best_header_height += delta;
    }
    let MnList::Supplied(list) = &mut ctx.mn_list else {
        panic!("a crafted case supplies its list");
    };
    let tip = i32::from_le_bytes(list[32..36].try_into().unwrap()) + delta;
    list[32..36].copy_from_slice(&tip.to_le_bytes());
    let (header, txs) = split_block(block).unwrap();
    let mut coinbase = Transaction::decode(&mut &txs[0][..]).unwrap();
    coinbase.extra_payload[2..6].copy_from_slice(&height.to_le_bytes());
    let mut bytes = Vec::new();
    coinbase.encode(&mut bytes);
    let mut moved: Vec<&[u8]> = vec![&bytes];
    moved.extend(txs[1..].iter().filter(|tx| {
        Transaction::decode(&mut &tx[..]).unwrap().tx_type != TxType::QuorumCommitment
    }));
    (ctx, assemble(header, &moved))
}

/// At a 576th height Core's `ProcessBlock` writes a snapshot row: the accepted crafted blocks
/// moved there agree with ours on it, the diff row and the resets.
#[test]
fn snapshot_rows_match_at_a_576th_height() {
    let oracle = Oracle::new(Pin::V24_0);
    let (mut accepted, mut outcomes) = (0, BTreeMap::new());
    for case in cases() {
        let (ctx, block) = moved_to(&case.ctx, &case.block, 576);
        let chain = case.chain();
        let (core, effects) = oracle.dml_block_effects(&chain, &block, &ctx, false, false).unwrap();
        if core.verdict == Verdict::Accept && core.list.is_some() {
            assert!(effects.snapshot_row.is_some(), "{}: Core's snapshot row", case.name);
            accepted += 1;
        }
        let outcome = compare_dml_block(&oracle, &chain, &case.params, &ctx, &block)
            .unwrap_or_else(|d| panic!("{} at 576: {d}", case.name));
        tally(&mut outcomes, format!("{outcome:?}"));
    }
    eprintln!("at 576: {outcomes:?}, {accepted} snapshot rows");
    assert!(accepted >= 5, "{outcomes:?}");
    assert_eq!(outcomes.get("BothAccept"), Some(&accepted), "{outcomes:?}");
}

/// The empty default list `CDeterministicMNList()`, as evodb stores it: a null block hash and
/// height -1. It is the one list whose height is not the tip's (the shim refuses any other).
fn default_list() -> MnList {
    let mut bytes = vec![0; 32];
    bytes.extend([0xff; 4]);
    bytes.extend([0; 5]);
    MnList::Supplied(bytes)
}

/// Over the default list a block's height is still the tip's + 1 (`pindex->nHeight`; EV2b r3
/// review), not the list's + 1 (0, a 576th height): each crafted case's coinbase alone, over the
/// default list at its regtest height (512–524), is accepted with no snapshot row on both sides.
#[test]
fn a_default_list_takes_the_tips_height() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut outcomes = BTreeMap::new();
    for case in cases() {
        let mut ctx = case.ctx.clone();
        ctx.mn_list = default_list();
        let (header, txs) = split_block(&case.block).unwrap();
        let block = assemble(header, &txs[..1]);
        let chain = case.chain();
        let (core, effects) = oracle.dml_block_effects(&chain, &block, &ctx, false, false).unwrap();
        if core.verdict == Verdict::Accept && core.list.is_some() {
            assert!(effects.snapshot_row.is_none(), "{}: Core's snapshot row", case.name);
        }
        let outcome = compare_dml_block(&oracle, &chain, &case.params, &ctx, &block)
            .unwrap_or_else(|d| panic!("{} over the default list: {d}", case.name));
        tally(&mut outcomes, format!("{outcome:?}"));
    }
    eprintln!("over the default list: {outcomes:?}");
    assert!(outcomes.get("BothAccept").is_some_and(|&n| n >= 5), "{outcomes:?}");
}

/// `ProcessBlock`'s own rejection (EV2b r2 review): at testnet's DIP3 enforcement height a block
/// that is not the pinned `DIP0003EnforcementHash` is `bad-dip3-enf-block` after the build
/// (`deterministicmns.cpp` v24:777), with no rows kept and no resets; both sides reject it there. The real
/// block passes.
#[test]
fn process_block_rejects_another_dip3_enforcement_block() {
    let oracle = Oracle::new(Pin::V24_0);
    let case = testnet_case();
    let ctx = &case.ctx;
    let compare = |block: &[u8]| compare_dml_block(&oracle, &Chain::test(), testnet(), ctx, block);
    assert_eq!(compare(&case.block), Ok(Outcome::BothAccept));

    let mut other = case.block.clone();
    other[76] ^= 1; // the nonce: another block hash
    let (core, effects) =
        oracle.dml_block_effects(&Chain::test(), &other, ctx, false, false).unwrap();
    assert_eq!(core.stage, Some(DmlStage::Process));
    assert!(matches!(&core.verdict, Verdict::Reject(r) if r.reason == "bad-dip3-enf-block"));
    assert_eq!(effects, DmlEffects::default());
    assert_eq!(compare(&other), Ok(Outcome::BothReject));
}

/// `step` of the scaffold's table for `tx_type` as the check layer names it: a ported type's
/// table (EV1-3: ProRegTx, ProUpRegTx; EV1-4: ProUpServTx, ProUpRevTx) has one `GVP-T` row (`GetValidatedPayload` with
/// `IsTriviallyValid`) and one `C` row (the contextual checks), dash-dml's vectors pinning the
/// steps within them.
fn step_of(tx_type: TxType, step: &'static str) -> &'static str {
    match tx_type {
        TxType::ProviderRegister
        | TxType::ProviderUpdateService
        | TxType::ProviderUpdateRegistrar
        | TxType::ProviderUpdateRevoke
            if step.starts_with('C') =>
        {
            "C"
        }
        TxType::ProviderRegister
        | TxType::ProviderUpdateService
        | TxType::ProviderUpdateRegistrar
        | TxType::ProviderUpdateRevoke => "GVP-T",
        _ => step,
    }
}

/// The unknown-version predicate the build and the check layer share (D56), against Core's
/// verdict: every crafted provider transaction of types 1 to 4 with a payload of only its version
/// (0, 4, 200, 0xffff) is one Core's check loop rejects (`bad-protx-version`: `IsTriviallyValid`
/// for 0, `GetValidatedPayload`'s maximum above it) and the predicate returns that version; with
/// a byte after it, `GetTxPayload` refuses it (`bad-protx-payload`) and the predicate is `None`.
/// Both sides reject it alike at that step. Kills Sol r2's mutant A, the predicate returning
/// `None` (the check layer then reads `bad-protx-payload`).
#[test]
fn the_unknown_version_predicate_matches_cores_verdict() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut seen = BTreeMap::new();
    for case in cases() {
        let (header, txs) = split_block(&case.block).unwrap();
        for tx in &txs[1..] {
            let mut tx = Transaction::decode(&mut &tx[..]).unwrap();
            if !(1..=4).contains(&tx.tx_type.to_base()) {
                continue;
            }
            for (version, trailing) in
                [0u16, 4, 200, u16::MAX].into_iter().flat_map(|v| [(v, false), (v, true)])
            {
                tx.extra_payload = version.to_le_bytes().to_vec();
                if trailing {
                    tx.extra_payload.push(0);
                }
                let mut bytes = Vec::new();
                tx.encode(&mut bytes);
                let ours = decode_tx(&mut &bytes[..]).unwrap().into_value();
                assert_eq!(unknown_pro_tx_version(&ours), (!trailing).then_some(version));
                let block = assemble(header, &[txs[0], &bytes]);
                let (core, _) = oracle
                    .dml_block_effects(&case.chain(), &block, &case.ctx, false, false)
                    .unwrap();
                let reason = if trailing {
                    "bad-protx-payload"
                } else {
                    "bad-protx-version"
                };
                assert_eq!(core.stage, Some(DmlStage::Check), "{}", case.name);
                assert!(matches!(&core.verdict, Verdict::Reject(r) if r.reason == reason));
                let detailed = compare_dml_block_detailed(
                    &oracle,
                    &case.chain(),
                    &case.params,
                    &case.ctx,
                    &block,
                )
                .unwrap_or_else(|d| panic!("{}: {d}", case.name));
                let step = match (trailing, version) {
                    (true, _) => "GVP 2",
                    (false, 0) => "T1",
                    _ => "GVP 3",
                };
                let step = step_of(tx.tx_type, step);
                assert_eq!(detailed.outcome, Outcome::BothReject, "{}", case.name);
                assert!(
                    matches!(detailed.check, Some(CheckOutcome::Exact { tx: 1, at, .. }) if at == step),
                    "{}: {:?}",
                    case.name,
                    detailed.check
                );
                tally(&mut seen, format!("{:?} {step} {reason}", tx.tx_type));
            }
        }
    }
    eprintln!("unknown versions: {seen:?}");
    assert!(seen.len() >= 6, "{seen:?}");
}

/// The network info rc.4 reads in a crafted ProRegTx or ProUpServTx made version 3 (extended)
/// (`providertx.h` rc.4:162-183, 269-289), compared with Core's verdict on the transaction. A
/// repeated purpose, which neither rc.4 nor base-sdk's decoder reads, is `bad-protx-payload` at
/// GVP 2 (CE4-01, `netinfo.h` rc.4:419-437). DEC-461: D5's "a..b" is
/// cleared to the invalid entry, `bad-protx-netinfo-bad` with V24; D5b's "a.b" is a valid domain,
/// so in the Core P2P group it is `bad-protx-netinfo-entry` with V24 (`netinfo.cpp` rc.4:90-113,
/// 585-623); before V24, version 3 is `bad-protx-version` at GVP 3.
#[test]
fn extended_net_info_matches_cores_verdict() {
    let oracle = Oracle::new(Pin::V24_0);
    // 1.1.1.1:19999, a service entry.
    let svc = [0x01u8, 0x01, 0x04, 1, 1, 1, 1, 0x4e, 0x1f];
    let domain = |name: &str| [&[2u8, name.len() as u8][..], name.as_bytes(), &[1, 0xbb]].concat();
    let canonical = [&[1u8, 1, 0, 1][..], &svc].concat();
    let variants: [(&str, Vec<u8>); 3] = [
        ("repeated", [&[1u8, 2, 0, 1][..], &svc, &[0, 1], &svc].concat()),
        ("D5", [&[1u8, 1, 0, 2][..], &domain("a..b"), &svc].concat()),
        ("D5b", [&[1u8, 1, 0, 2][..], &domain("a.b"), &svc].concat()),
    ];
    let extended = || NetInfo::Extended(NetInfoV2::decode(&mut &canonical[..]).unwrap());
    let mut seen = BTreeMap::new();
    for case in cases() {
        let (header, txs) = split_block(&case.block).unwrap();
        for tx in &txs[1..] {
            let mut tx = Transaction::decode(&mut &tx[..]).unwrap();
            // The extended payload and where its network info starts (base-sdk refuses some
            // crafted ones, e.g. a malleable BLS key).
            let (payload, at) = match tx.tx_type {
                TxType::ProviderRegister => (
                    reencoded(&tx.extra_payload, |p: &mut ProRegTx| {
                        (p.version, p.net_info) = (ProTxVersion::ExtAddr, extended());
                    }),
                    42,
                ),
                TxType::ProviderUpdateService => (
                    reencoded(&tx.extra_payload, |p: &mut ProUpServTx| {
                        (p.version, p.net_info) = (ProTxVersion::ExtAddr, extended());
                    }),
                    36,
                ),
                _ => continue,
            };
            let Some(payload) = payload else {
                continue;
            };
            assert_eq!(payload[at..at + canonical.len()], canonical[..], "{}", case.name);
            for (name, net_info) in &variants {
                tx.extra_payload =
                    [&payload[..at], net_info, &payload[at + canonical.len()..]].concat();
                let mut bytes = Vec::new();
                tx.encode(&mut bytes);
                let block = assemble(header, &[txs[0], &bytes]);
                let (core, _) = oracle
                    .dml_block_effects(&case.chain(), &block, &case.ctx, false, false)
                    .unwrap();
                let (reason, step) = match (*name, case.v24) {
                    ("repeated", _) => ("bad-protx-payload", "GVP 2"),
                    (_, false) => ("bad-protx-version", "GVP 3"),
                    ("D5", true) => ("bad-protx-netinfo-bad", "C4"),
                    _ => ("bad-protx-netinfo-entry", "C4"),
                };
                let step = step_of(tx.tx_type, step);
                let label = format!("{} {:?} {name}", case.name, tx.tx_type);
                let Verdict::Reject(r) = &core.verdict else {
                    panic!("{label}: {:?}", core.verdict);
                };
                assert_eq!(r.reason, reason, "{label}");
                let detailed = compare_dml_block_detailed(
                    &oracle,
                    &case.chain(),
                    &case.params,
                    &case.ctx,
                    &block,
                )
                .unwrap_or_else(|d| panic!("{label}: {d}"));
                assert_eq!(detailed.outcome, Outcome::BothReject, "{label}");
                let Some(CheckOutcome::Exact {
                    tx: 1,
                    at,
                    ..
                }) = &detailed.check
                else {
                    panic!("{label}: {:?}", detailed.check);
                };
                assert_eq!(*at, step, "{label}");
                tally(&mut seen, format!("{:?} {name} v24={}", tx.tx_type, case.v24));
            }
        }
    }
    eprintln!("extended net info: {seen:?}");
    // The crafted cases hold no ProUpServTx with V24 active.
    assert_eq!(seen.len(), 9, "{seen:?}");
}

/// `bytes` decoded as `P`, edited and encoded again; `None` where base-sdk refuses them.
fn reencoded<P: BaseCodec>(bytes: &[u8], edit: impl FnOnce(&mut P)) -> Option<Vec<u8>> {
    let mut p = P::decode(&mut &bytes[..]).ok()?;
    edit(&mut p);
    let mut out = Vec::new();
    p.encode(&mut out);
    Some(out)
}

/// A raw input (`diff_dml_apply`'s layout) of one kind-7 transaction: `raw_type` indexes the
/// generator's raw types (ProRegTx 0, ProUpRevTx 3), with `payload` as its extra payload.
fn raw_input(case: usize, raw_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut input = vec![case as u8, 0, 7, raw_type, u8::try_from(payload.len()).unwrap()];
    input.extend(payload);
    input
}

/// The first crafted provider transaction of `tx_type` in a case `pick` accepts, with the case's
/// index.
fn crafted_payload(tx_type: TxType, pick: impl Fn(&Case) -> bool) -> (usize, Vec<u8>) {
    cases()
        .iter()
        .enumerate()
        .filter(|(_, case)| pick(case))
        .find_map(|(i, case)| {
            let (_, txs) = split_block(&case.block).unwrap();
            txs.iter()
                .map(|tx| Transaction::decode(&mut &tx[..]).unwrap())
                .find(|tx| tx.tx_type == tx_type)
                .map(|tx| (i, tx.extra_payload))
        })
        .unwrap()
}

/// D56's check-layer mutants and the inputs that show each, compared exactly at the step the mutant
/// changes. A: the shared unknown-version predicate (`unknown_pro_tx_version`, the check layer's
/// GVP 3 and the build's out-of-domain refusal) answering `None`; a change to the build's use alone
/// is behind the check loop's exact rejection, so unreachable here and intentionally unobserved
/// (Sol r4). B: `GetMax`'s V19-only arm (`version_max`) answering 3; C: the operator reward bound
/// (T12) off by one; D: the netinfo owner checks (C8, C9) finding no entry held. Each makes
/// `compare_block`, the fuzz entry, diverge on a raw and a checked input: A's and D's raw ones are
/// generated, B's and C's edit a crafted payload, which the random raw corpus does not reach
/// (Core's checks stop it earlier). E: an invalid entry, which rc.4 rejects at the network-fields
/// step (C4, C2) with the reason rc.3's entry loop (T11, T7) gave, and an invalid entry with an
/// operator reward over 10000 is the reward's reject, which rc.4 now checks first (CE4-03). D57
/// (Opus-h r1): SHAREDMN, the registrar update's masternode-of-shares check (C3) off, and EVOVER,
/// the service update's Evo version bound (C6) `< 2`, each on a checked probe seed.
#[test]
fn check_layer_mutant_inputs_compare_exactly() {
    let oracle = Oracle::new(Pin::V24_0);
    let hex = |s: &str| -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    };
    // B: a ProUpRevTx of version 3 before V24 (`GetMax` is 2 there).
    let (case, payload) = crafted_payload(TxType::ProviderUpdateRevoke, |c| !c.v24);
    let mut p = ProUpRevTx::decode(&mut &payload[..]).unwrap();
    p.version = ProTxVersion::ExtAddr;
    let mut b = Vec::new();
    p.encode(&mut b);
    let b = raw_input(case, 3, &b);
    // C: a ProRegTx paying its operator 10001 (out of 10000), unsigned to fit a raw payload.
    let (case, payload) = crafted_payload(TxType::ProviderRegister, |_| true);
    let mut p = ProRegTx::decode(&mut &payload[..]).unwrap();
    p.operator_reward = 10_001;
    p.vch_sig = CollateralSig::default();
    let mut c = Vec::new();
    p.encode(&mut c);
    let c = raw_input(case, 0, &c);
    // CE4-03 (ce.md §5 item 8): C's payload made version 3 on a V24 case with an invalid
    // (unknown-type) entry in its network info, so it holds two faults. rc.4 checks the operator
    // reward (T12) before the network fields, where rc.3's entry loop came first and gave
    // `bad-protx-netinfo-bad`, the entry's reject with the reward in bounds.
    let (case, payload) = crafted_payload(TxType::ProviderRegister, |c| c.v24);
    let svc = [0x01u8, 0x01, 0x04, 1, 1, 1, 1, 0x4e, 0x1f];
    let canonical = [&[1u8, 1, 0, 1][..], &svc].concat();
    let invalid_entry = |operator_reward| {
        let mut p = ProRegTx::decode(&mut &payload[..]).unwrap();
        let net_info = NetInfo::Extended(NetInfoV2::decode(&mut &canonical[..]).unwrap());
        (p.version, p.net_info, p.operator_reward) =
            (ProTxVersion::ExtAddr, net_info, operator_reward);
        p.vch_sig = CollateralSig::default();
        let mut out = Vec::new();
        p.encode(&mut out);
        assert_eq!(out[42..42 + canonical.len()], canonical[..]);
        raw_input(
            case,
            0,
            &[&out[..42], &[1, 1, 0, 1, 0x03], &out[42 + canonical.len()..]].concat(),
        )
    };
    for (operator_reward, expect) in
        [(10_001, "bad-protx-operator-reward"), (10_000, "bad-protx-netinfo-bad")]
    {
        let g = generate(&invalid_entry(operator_reward)).unwrap();
        let detailed =
            compare_dml_block_detailed(&oracle, &g.chain(), &g.case.params, &g.ctx, &g.block)
                .unwrap();
        let reason = detailed.core.as_ref().map(|(_, reason)| reason.as_str());
        assert_eq!(reason, Some(expect), "CE4-03 {operator_reward}: {:?}", detailed.core);
    }
    let two = invalid_entry(10_001);
    let inputs = [
        ("A raw", hex("5823c5cd43d2b4"), "GVP 3"),
        ("A raw", hex("8e20b96a3cc5759c7000e3"), "GVP 3"),
        ("A checked", hex("0d0c4006"), "GVP 3"),
        ("B raw", b, "GVP 3"),
        ("B checked", hex("060c2806"), "GVP 3"),
        ("C raw", c, "T12"),
        ("C checked", hex("070c1000"), "T12"),
        ("CE4-03 raw", two, "T12"),
        ("D raw", hex("67e14696afebc1e738"), "C9"),
        ("D checked", hex("030c3800"), "C8"),
        ("D checked", hex("000c3804"), "C9"),
        ("E checked", hex("000e74000101010101010101"), "C4"),
        ("E checked", hex("000e74040101010101010101"), "C2"),
        ("SHAREDMN checked", hex("0f0c5c05"), "C3"),
        ("EVOVER checked", hex("000e7004"), "C6"),
    ];
    for (name, input, step) in inputs {
        assert_eq!(compare_block(&input, Pin::V24_0), Ok(Outcome::BothReject), "{name}");
        let detailed = if input[1] & FLAG_CHECKED == 0 {
            let g = generate(&input).unwrap();
            compare_dml_block_detailed(&oracle, &g.chain(), &g.case.params, &g.ctx, &g.block)
        } else {
            compare_sequence(&oracle, &input).map(|blocks| blocks.last().unwrap().detailed.clone())
        };
        let check = detailed.unwrap_or_else(|d| panic!("{name}: {d}")).check;
        assert!(
            matches!(check, Some(CheckOutcome::Exact { at, tx_type, .. }) if at == step_of(tx_type, step)),
            "{name}: {check:?}"
        );
    }
}

/// D57 (Opus-h r1 nit 2): a checked sequence whose third block spends a masternode of shares'
/// collateral (`bad-shared-collateral-spend`, a build rejection). The checked generator reaches this
/// rule only by chance, here in a random probed round; with the rule off (mutant SHSPEND) the block
/// diverges.
#[test]
fn a_shared_collateral_spend_is_compared() {
    let s = concat!(
        "dbdce1cb10b49a930a9d38d02a598c3296d57ba3142e15fb247ade455cce218d3a27e09a0dfb87b2712d835a",
        "8594cdcc3b160ad528f73d66963e23ee450b2faa1edeb1e27df182e7003b8907e71f524ca09c11ef8a8ad38b",
        "7734961daa73f77cdb4cbfcc3d314c6d71cf95e46a12669f7bf370c6ce12c8b4b94ce1c20f63ce70cb5f89a4",
        "45c98a92a76c28a84631cdc44380b9b83bb55b9a2951b19092ced95b833f49a65fabccf344a3916c6b1a0c91",
        "41560138aea5b32f7727f582251b1b4eee5628",
    );
    let input: Vec<u8> =
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    assert_eq!(input[1] & (FLAG_CHECKED | FLAG_PROBE), FLAG_CHECKED | FLAG_PROBE);
    assert_eq!(compare_block(&input, Pin::V24_0), Ok(Outcome::BothAccept));
    let blocks = compare_sequence(&Oracle::new(Pin::V24_0), &input).unwrap();
    let core: Vec<_> = blocks.iter().map(|b| b.detailed.core.clone()).collect();
    assert_eq!(core[2], Some((DmlStage::Build, "bad-shared-collateral-spend".to_owned())));
    assert_eq!(blocks[2].detailed.outcome, Outcome::BothReject);
}

/// A null commitment naming a block not in the chain: Core's check loop rejects it before its
/// null return (`bad-qc-quorum-hash`, `commitment.cpp` v24:286-294), at a step the node lacks
/// until EV1-5 (D56): out of domain, not compared.
#[test]
fn a_null_commitment_of_an_unknown_quorum_is_a_missing_check() {
    let case = cases().iter().find(|c| c.name == "qc_wrong_quorum_hash").unwrap();
    let detailed = compare_dml_block_detailed(
        &Oracle::new(Pin::V24_0),
        &case.chain(),
        &case.params,
        &case.ctx,
        &case.block,
    )
    .unwrap();
    assert_eq!(detailed.outcome, Outcome::OutOfDomain);
    assert_eq!(detailed.core, Some((DmlStage::Check, String::from("bad-qc-quorum-hash"))));
    assert!(matches!(
        detailed.check,
        Some(CheckOutcome::OutOfDomain {
            tx_type: TxType::QuorumCommitment,
            at: "§3.11",
            ..
        })
    ));
}

/// Every step of the check tables (D56): ids unique in their table, reasons Core's (EV1's census
/// constants), and every missing step owned by an EV1 slice, with BS11a or BS12 for BLS validity.
#[test]
fn check_tables_are_consistent() {
    let census: BTreeSet<&str> = CLASSES.iter().map(|(r, _)| *r).collect();
    for table in TABLES {
        let mut ids = BTreeSet::new();
        for step in table.steps {
            assert!(ids.insert(step.id), "{} {}: twice", table.name, step.id);
            assert!(!step.reasons.is_empty(), "{} {}", table.name, step.id);
            for reason in step.reasons {
                assert!(census.contains(reason), "{} {}: {reason}", table.name, step.id);
            }
            if let Some(owner) = &step.owner {
                let mut parts = owner.slice.split(' ');
                assert!(
                    ["EV1-2", "EV1-3", "EV1-4", "EV1-5"].contains(&parts.next().unwrap()),
                    "{} {}: {}",
                    table.name,
                    step.id,
                    owner.slice
                );
                assert!(parts.all(|p| ["BS11a", "BS12"].contains(&p)), "{}", owner.slice);
            }
        }
    }
    // Along a transaction's walk (the dispatcher, then its type's table) no reason is both a step
    // we have and one we lack: where it were, Core's reason would not place its rejection, and a
    // wrong step of ours could pass as the one we lack (Opus H1).
    for table in TABLES.iter().filter(|t| ![COVENANT.name, DISPATCH.name].contains(&t.name)) {
        let steps = || DISPATCH.steps.iter().chain(table.steps);
        for ours in steps().filter(|s| s.owner.is_none()) {
            for lacking in steps().filter(|s| s.owner.is_some()) {
                let shared = ours.reasons.iter().find(|r| lacking.reasons.contains(r));
                assert!(shared.is_none(), "{} {} {}: {shared:?}", table.name, ours.id, lacking.id);
            }
        }
    }
    // Each EV1 slice lowers this as it retires its rows (EV1-2: 35 to 27; EV1-3: 27 to 10,
    // with Core's catch moved from the dispatcher to the first step of types 2 and 4; EV1-4:
    // 10 to 5, types 2 and 4 with Core's catch in their `C` step).
    assert!(missing_checks().count() >= 5);
}

/// The source of `dir` and its subdirectories.
fn sources(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            text.push_str(&sources(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            text.push_str(&std::fs::read_to_string(&path).unwrap());
        }
    }
    text
}

/// A stale missing step (D56): once the EV1 code it waits for lands in dash-dml (its file of
/// `specialtx/`, or its function there), this fails until the step is wired into `dml_checks`
/// with the exact comparison and its row deleted, as that slice's acceptance.
#[test]
fn missing_checks_have_not_landed() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../dash-dml/src/specialtx");
    let text = sources(&dir);
    for (table, step) in missing_checks() {
        let owner = step.owner.as_ref().unwrap();
        let landed = match owner.lands.strip_prefix("fn ") {
            Some(_) => text.contains(owner.lands),
            None => dir.join(owner.lands).exists(),
        };
        assert!(
            !landed,
            "{} {}: {} landed ({}): wire it into dml_checks and delete the row",
            table.name, step.id, owner.slice, owner.lands
        );
    }
}

/// The check layer's comparisons (D56) of a run: exact, by our table and step; out of domain, by
/// Core's reason, the rejected transaction's type and the step we lack; before DIP3.
#[derive(Default)]
struct CheckTally {
    exact: BTreeMap<String, usize>,
    missing: BTreeMap<String, usize>,
    ambiguous: BTreeMap<String, usize>,
    pre_dip3: usize,
}

impl CheckTally {
    fn record(&mut self, d: &Detailed) {
        let Some((DmlStage::Check, reason)) = &d.core else {
            return;
        };
        match d.check {
            _ if d.pre_dip3 => self.pre_dip3 += 1,
            Some(CheckOutcome::Exact {
                table,
                at,
                ..
            }) => tally(&mut self.exact, format!("{table} {at} {reason}")),
            Some(CheckOutcome::OutOfDomain {
                tx_type,
                at,
                ..
            }) => tally(&mut self.missing, format!("{reason} | {tx_type:?} | {at}")),
            Some(CheckOutcome::Ambiguous {
                tx_type,
                at,
                ours,
                ..
            }) => {
                tally(&mut self.ambiguous, format!("{reason} | {tx_type:?} | {at} or from {ours}"))
            }
            _ => panic!("a check rejection without a check comparison"),
        }
    }

    fn exact(&self) -> usize {
        self.exact.values().sum()
    }

    fn missing(&self) -> usize {
        self.missing.values().sum()
    }

    fn ambiguous(&self) -> usize {
        self.ambiguous.values().sum()
    }

    fn report(&self, label: &str) {
        let (missing, total) =
            (self.missing(), self.exact() + self.missing() + self.ambiguous() + self.pre_dip3);
        eprintln!(
            "{label}: Core's check rejections {total}: {} compared exactly, {missing} out of \
             domain, {} ambiguous, {} before DIP3\n  exact: {:?}\n  out of domain: {:?}\n  \
             ambiguous: {:?}",
            self.exact(),
            self.ambiguous(),
            self.pre_dip3,
            self.exact,
            self.missing,
            self.ambiguous
        );
    }
}

/// splitmix64.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// A random checked-sequence input of 2 to 401 bytes.
fn random_input(state: &mut u64) -> Vec<u8> {
    let len = 2 + (next(state) % 400) as usize;
    (0..len).map(|_| next(state) as u8).collect()
}

/// `input` through [`compare_sequence`], which must agree with the target's entry on the last
/// block's outcome.
fn compare_checked(oracle: &Oracle, round: usize, input: &[u8]) -> Vec<SequenceBlock> {
    let blocks = compare_sequence(oracle, input)
        .unwrap_or_else(|d| panic!("round {round} {input:02x?}: {d}"));
    let last = blocks.last().map_or(Outcome::OutOfDomain, |b| b.detailed.outcome);
    assert_eq!(compare_block(input, Pin::V24_0), Ok(last), "round {round}");
    blocks
}

/// `diff_dml_block` over raw `inputs` (D53, D56): [`FLAG_CHECKED`] cleared, as `diff_dml_apply`'s
/// generator reads them (Sol r3 F2), each through the target's own entry too. No divergence, at
/// least 30 accepted by both, and Core's check rejections `[exact, out of domain, ambiguous]`: at
/// least `bounds[0]` compared exactly, at most the others (a ceiling on what is not compared);
/// every out-of-domain outcome is a step the node lacks.
fn run_inputs(label: &str, inputs: impl Iterator<Item = (String, Vec<u8>)>, bounds: [usize; 3]) {
    let oracle = Oracle::new(Pin::V24_0);
    let (mut outcomes, mut checks) = (BTreeMap::new(), CheckTally::default());
    for (name, mut input) in inputs {
        if let Some(flags) = input.get_mut(1) {
            *flags &= !FLAG_CHECKED;
        }
        let Some(g) = generate(&input) else {
            tally(&mut outcomes, String::from("no input"));
            continue;
        };
        let detailed =
            compare_dml_block_detailed(&oracle, &g.chain(), &g.case.params, &g.ctx, &g.block)
                .unwrap_or_else(|d| panic!("{name} {input:02x?}: {d}"));
        assert_eq!(compare_block(&input, Pin::V24_0), Ok(detailed.outcome), "{name}");
        tally(&mut outcomes, outcome_label(&detailed));
        checks.record(&detailed);
        if detailed.outcome == Outcome::OutOfDomain {
            assert!(
                matches!(
                    detailed.check,
                    Some(CheckOutcome::OutOfDomain { .. } | CheckOutcome::Ambiguous { .. })
                ),
                "{name} {input:02x?}"
            );
        }
    }
    eprintln!("{label}: {outcomes:?}");
    checks.report(label);
    assert!(outcomes.get("BothAccept").is_some_and(|&n| n >= 30), "{outcomes:?}");
    assert!(checks.exact() >= bounds[0], "{}", checks.exact());
    assert!(
        checks.missing() <= bounds[1] && checks.ambiguous() <= bounds[2],
        "{} {}",
        checks.missing(),
        checks.ambiguous()
    );
}

/// Random generated inputs agree. `diff_dml_apply`'s generator does not aim at Core's checks
/// (`oracle_dml_apply` has none), so most stop there (D41), where the check layers compare.
#[test]
fn generated_blocks_match_the_checked_path() {
    let mut state = 0x0e2b_0002;
    let inputs = (0..600).map(|round| {
        let len = 2 + (next(&mut state) % 40) as usize;
        (format!("round {round}"), (0..len).map(|_| next(&mut state) as u8).collect())
    });
    run_inputs("generated", inputs, [190, 350, 9]);
}

/// `diff_dml_apply`'s public seeds agree too.
#[test]
fn public_seeds_match_the_checked_path() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../node-fuzz/public-seeds/diff_dml_apply");
    let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    paths.sort();
    let label = format!("{} seeds", paths.len());
    run_inputs(
        &label,
        paths.into_iter().map(|p| (p.display().to_string(), std::fs::read(p).unwrap())),
        [90, 200, 3],
    );
}

/// Checked sequences (EV2b D54; Sol r2 F2, F3) agree block by block, our lists carried from
/// block to block: the generated blocks reach the build's and `ProcessBlock`'s rejections, not
/// only acceptance, with 0 to 50 transactions; and blocks both sides accept retake values an
/// earlier block freed, where an entry our unique-property map kept would show. The probe bit
/// cleared, these are D54's sequences; the target's entry reports each one's last block.
#[test]
fn checked_sequences_match() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut state = 0x0e2b_0054;
    let (mut outcomes, mut reasons, mut sizes) =
        (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let (mut retaken, mut checks) = (0, CheckTally::default());
    for round in 0..300 {
        let mut input = random_input(&mut state);
        input[1] = (input[1] | FLAG_CHECKED) & !FLAG_PROBE;
        for block in compare_checked(&oracle, round, &input) {
            let d = block.detailed;
            tally(&mut outcomes, outcome_label(&d));
            checks.record(&d);
            if let Some((stage, reason)) = &d.core {
                tally(&mut reasons, format!("{stage:?} {reason}"));
            }
            tally(&mut sizes, format!("{}+", block.txs / 10 * 10));
            retaken += usize::from(d.outcome == Outcome::BothAccept && block.retaken > 0);
        }
    }
    eprintln!(
        "checked: {outcomes:?}\n  Core's rejections: {reasons:?}\n  transactions: {sizes:?}\n  \
         accepted blocks retaking freed values: {retaken}"
    );
    checks.report("checked");
    assert_eq!((checks.missing(), checks.ambiguous()), (0, 0), "checked");
    let count = |key: &str| outcomes.get(key).copied().unwrap_or_default();
    assert!(count("BothAccept") >= 250, "{outcomes:?}");
    assert!(count("BothReject Build") >= 250, "{outcomes:?}");
    assert!(count("BothReject Process") >= 15, "{outcomes:?}");
    let build_reasons = reasons.keys().filter(|r| r.starts_with("Build ")).count();
    assert!(build_reasons >= 5, "{reasons:?}");
    assert!(sizes.contains_key("0+") && sizes.contains_key("40+"), "{sizes:?}");
    assert!(retaken >= 10, "{retaken}");
}

/// Checked sequences with probes (D56): transactions changed so that Core's check loop rejects
/// them at steps the node implements, compared exactly at the rejected transaction and step, in
/// every family (the dispatcher, `GetValidatedPayload`, `IsTriviallyValid`, the type's checks).
#[test]
fn probed_sequences_match() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut state = 0x0e2b_0056;
    let (mut outcomes, mut checks) = (BTreeMap::new(), CheckTally::default());
    for round in 0..300 {
        let mut input = random_input(&mut state);
        input[1] |= FLAG_CHECKED | FLAG_PROBE;
        for block in compare_checked(&oracle, round, &input) {
            tally(&mut outcomes, outcome_label(&block.detailed));
            checks.record(&block.detailed);
        }
    }
    eprintln!("probed: {outcomes:?}");
    checks.report("probed");
    assert_eq!((checks.missing(), checks.ambiguous()), (0, 0), "probed");
    assert!(outcomes.get("BothAccept").is_some_and(|&n| n >= 50), "{outcomes:?}");
    assert_eq!(unreached(&checks), PROBED_MISSES);
}

/// The steps of ours a probe reaches (D56, D57; Opus tests), by table. The ported tables (EV1-3:
/// ProRegTx, ProUpRegTx; EV1-4: ProUpServTx, ProUpRevTx) have two rows each, [`step_of`].
const PROBED_STEPS: [(&str, &[&str]); 6] = [
    ("dispatcher (§3.2)", &["5"]),
    ("CbTx (§3.2 3.8-3.10, §3.10)", &["3.8"]),
    ("ProRegTx (§3.3)", &["GVP-T", "C"]),
    ("ProUpServTx (§3.4)", &["GVP-T", "C"]),
    ("ProUpRegTx (§3.5)", &["GVP-T", "C"]),
    ("ProUpRevTx (§3.6)", &["GVP-T", "C"]),
];

/// The [`PROBED_STEPS`] random probed sequences miss (their state is fixed), which the probe
/// seeds reach.
const PROBED_MISSES: [&str; 0] = [];

/// The probed steps with no exact check rejection in `checks`.
fn unreached(checks: &CheckTally) -> Vec<String> {
    let reached = |key: &str| checks.exact.keys().any(|k| k.starts_with(&format!("{key} ")));
    PROBED_STEPS
        .iter()
        .flat_map(|(table, steps)| steps.iter().map(move |step| format!("{table} {step}")))
        .filter(|key| !reached(key))
        .collect()
}

/// `diff_dml_block`'s own public seeds, checked sequences, agree block by block; its probe seeds
/// reach an exact check rejection in each family.
#[test]
fn checked_public_seeds_match() {
    let oracle = Oracle::new(Pin::V24_0);
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../node-fuzz/public-seeds/diff_dml_block");
    let (mut outcomes, mut checks, mut probes) =
        (BTreeMap::new(), CheckTally::default(), CheckTally::default());
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let input = std::fs::read(&path).unwrap();
        assert_ne!(input[1] & FLAG_CHECKED, 0, "{}", path.display());
        let blocks =
            compare_sequence(&oracle, &input).unwrap_or_else(|d| panic!("{}: {d}", path.display()));
        for block in blocks {
            tally(&mut outcomes, outcome_label(&block.detailed));
            checks.record(&block.detailed);
            if input[1] & FLAG_PROBE != 0 {
                probes.record(&block.detailed);
            }
        }
    }
    eprintln!("checked seeds: {outcomes:?}");
    probes.report("probe seeds");
    assert_eq!((checks.missing(), checks.ambiguous()), (0, 0), "checked seeds");
    for key in ["BothAccept", "BothReject Build", "BothReject Process", "BothReject Check"] {
        assert!(outcomes.contains_key(key), "{outcomes:?}");
    }
    let unreached = unreached(&probes);
    assert!(unreached.is_empty(), "{unreached:?}");
}

/// A value the first block of a sequence frees, which the second block takes again: both sides
/// accept both blocks. Found under a planted mutant (EV2b D54, M3) that keeps a changed
/// masternode's old values in our unique-property map, with I-U's map comparison off so `xcheck`
/// passes: there the second block diverges, and passes when our list is decoded from Core's.
#[test]
fn a_value_freed_by_one_block_is_free_in_the_next() {
    let input = [243, 21, 42, 120, 52, 229, 197, 57, 8, 82, 21, 239, 123, 20, 61, 66, 156, 193];
    assert_ne!(input[1] & FLAG_CHECKED, 0);
    let blocks = compare_sequence(&Oracle::new(Pin::V24_0), &input).unwrap();
    let outcomes: Vec<_> = blocks.iter().map(|b| (b.detailed.outcome, b.retaken)).collect();
    assert!(
        matches!(outcomes[..2], [(Outcome::BothAccept, _), (Outcome::BothAccept, 1..)]),
        "{outcomes:?}"
    );
}

/// The blocks a checked sequence generates as `compare_sequence` walks it: the context moves
/// past each block Core accepts with a list (`compare_sequence` carries on both sides' acceptance,
/// which the other tests assert agree). SHA-256 over every block's bytes.
fn sequence_digest(oracle: &Oracle, input: &[u8]) -> String {
    use bitcoin_hashes::{HashEngine, sha256};
    let mut engine = sha256::Hash::engine();
    if let Some(mut walk) = Walk::new(input) {
        while let Some(block) = walk.next_block() {
            engine.input(&(block.len() as u64).to_le_bytes());
            engine.input(&block);
            let Ok((core, _)) =
                oracle.dml_block_effects(&walk.chain(), &block, &walk.ctx, false, false)
            else {
                continue;
            };
            if let (Verdict::Accept, Some((canonical, _))) = (&core.verdict, &core.list) {
                walk.accepted(canonical).unwrap();
            }
        }
    }
    sha256::Hash::from_engine(engine).to_byte_array().iter().map(|b| format!("{b:02x}")).collect()
}

/// The regression input of `a_value_freed_by_one_block_is_free_in_the_next`.
const FREED_AND_RETAKEN: [u8; 18] =
    [243, 21, 42, 120, 52, 229, 197, 57, 8, 82, 21, 239, 123, 20, 61, 66, 156, 193];

/// EV2b D56: the checked generator gives every committed seed and the regression input the
/// blocks pinned in `data/dml-block-sequences.txt` (`DML_BLOCK_BLESS=1` rewrites it). The seeds
/// without the probe bit ([`FLAG_PROBE`]) and the regression input were pinned by the generator
/// before D56: with the bit off, an input generates what it did before the probe existed. The
/// probe seeds were pinned by D56's.
#[test]
fn checked_generator_is_byte_stable() {
    let oracle = Oracle::new(Pin::V24_0);
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../node-fuzz/public-seeds/diff_dml_block");
    let mut inputs: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            (path.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(path).unwrap())
        })
        .collect();
    inputs.sort();
    inputs.push((String::from("regression freed-and-retaken"), FREED_AND_RETAKEN.to_vec()));
    let mut lines = String::new();
    for (name, input) in inputs {
        lines.push_str(&format!("{name} {}\n", sequence_digest(&oracle, &input)));
    }
    let pinned = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/dml-block-sequences.txt");
    if std::env::var_os("DML_BLOCK_BLESS").is_some() {
        std::fs::write(&pinned, &lines).unwrap();
    }
    let expected = std::fs::read_to_string(&pinned).unwrap();
    for (ours, theirs) in lines.lines().zip(expected.lines()) {
        assert_eq!(ours, theirs);
    }
    assert_eq!(lines.lines().count(), expected.lines().count());
}
