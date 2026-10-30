//! L0 and L1 for EV2b: `build_new_list_from_block` against Core's `RebuildListFromBlock`.
//!
//! L0 replays O2a-EV's crafted regtest blocks (`dash-oracle-sys/tests/data/dml-crafted-v24.0.json`,
//! one per masternode-list operator of EV2 §6.5) whole and with their transactions one at a
//! time, so a case Core rejects early still reaches the later branches. L1 runs
//! `diff_dml_apply` over generated inputs.

use std::collections::{BTreeMap, BTreeSet};

use dash_dml::codec::core_disk;
use dash_dml::specialtx::reasons::BAD_SHARED_COLLATERAL_SPEND;
use dash_dml::testkit::replay::read_vectors;
use dash_oracle_sys::{MnList as CoreList, Oracle, Pin};
use dash_primitives::SpecialPayload;
use node_l1::Outcome;
use node_l1::dml::unhex;
use node_l1::dml_apply::{
    PROUPSHARE, PROUPSHAREDREG, SHARE_OWNER_DUP_KEY, SHARE_PATHS, SHAREDREG_DUP_KEY, assemble,
    cases, compare_apply, compare_dml_apply, core_verdict, generate, oracle_context, payload,
    share_paths, split_block, with_one_tx,
};

fn tally(outcomes: &mut BTreeMap<String, usize>, outcome: Outcome) {
    *outcomes.entry(format!("{outcome:?}")).or_default() += 1;
}

#[test]
fn crafted_blocks_match_the_oracle() {
    let oracle = Oracle::new(Pin::V24_0);
    let mut outcomes = BTreeMap::new();
    for case in cases() {
        let chain = case.chain();
        let block = &case.block;
        let mut blocks = vec![(String::from("whole"), block.clone())];
        for i in 1..usize::from(block[80]) {
            if let Some(single) = with_one_tx(block, i) {
                blocks.push((format!("tx {i}"), single));
            }
        }
        for (part, bytes) in blocks {
            let outcome = compare_dml_apply(&oracle, &chain, &case.params, &case.ctx, &bytes)
                .unwrap_or_else(|d| panic!("{} ({part}): {d}", case.name));
            tally(&mut outcomes, outcome);
        }
    }
    eprintln!("{outcomes:?}");
    // 68 crafted, and the derived shared case's coinbase-only block (EV2b D23).
    assert_eq!(outcomes.get("BothAccept"), Some(&69));
    assert_eq!(outcomes.get("BothReject"), Some(&10));
}

/// splitmix64.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Whether a generated input's block is V24 and holds a provider update (`SetStateVersion`'s
/// migrations).
fn has_v24_update(bytes: &[u8]) -> bool {
    generate(bytes).filter(|i| i.v24).is_some_and(|input| {
        input.txs().iter().any(|tx| {
            matches!(
                payload(tx),
                Some(
                    SpecialPayload::ProviderUpdateService(_)
                        | SpecialPayload::ProviderUpdateRegistrar(_)
                        | SpecialPayload::ProviderUpdateRevoke(_)
                )
            )
        })
    })
}

/// Random generated inputs reach both verdicts and agree, and V24 blocks with provider updates
/// are accepted (EV2b D22; the share paths are the seeds').
#[test]
fn generated_blocks_match_the_oracle() {
    let mut state = 0x0e2b_0001;
    let mut outcomes = BTreeMap::new();
    let mut v24_updates = 0;
    for round in 0..600 {
        let len = 2 + (next(&mut state) % 40) as usize;
        let input: Vec<u8> = (0..len).map(|_| next(&mut state) as u8).collect();
        let outcome = compare_apply(&input, Pin::V24_0)
            .unwrap_or_else(|d| panic!("round {round} {input:02x?}: {d}"));
        if outcome == Outcome::BothAccept && has_v24_update(&input) {
            v24_updates += 1;
        }
        tally(&mut outcomes, outcome);
    }
    eprintln!("{outcomes:?}, {v24_updates} accepted V24 blocks with provider updates");
    assert!(outcomes.get("BothAccept").is_some_and(|&n| n > 50), "{outcomes:?}");
    assert!(outcomes.get("BothReject").is_some_and(|&n| n > 50), "{outcomes:?}");
    assert!(v24_updates >= 5, "{v24_updates}");
    // Uncompared inputs stay rare (EV2b D20): payloads of an unknown version (D2), which the shim
    // refuses, and a QcTx without members.
    assert!(outcomes.get("OutOfDomain").is_none_or(|&n| n * 50 < 600), "{outcomes:?}");
}

/// `diff_dml_apply`'s public seeds (`node-fuzz/public-seeds/diff_dml_apply`) agree, most are
/// compared, and they reach every shared-masternode path (EV2b D22).
#[test]
fn public_seeds_match_the_oracle() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/diff_dml_apply");
    let mut outcomes = BTreeMap::new();
    let mut paths = BTreeSet::new();
    let mut dup_key = Vec::new();
    let mut held = BTreeSet::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let input = std::fs::read(&path).unwrap();
        let outcome = compare_apply(&input, Pin::V24_0)
            .unwrap_or_else(|d| panic!("{} diverges: {d}", path.display()));
        if outcome != Outcome::OutOfDomain {
            let reached = share_paths(&input);
            if from_held_shared_mn(&input) {
                held.extend(reached.iter().copied());
            }
            if reached.iter().any(|p| [SHARE_OWNER_DUP_KEY, SHAREDREG_DUP_KEY].contains(p)) {
                dup_key.push(input.clone());
            }
            paths.extend(reached);
        }
        tally(&mut outcomes, outcome);
    }
    eprintln!("{outcomes:?} {paths:?}");
    for input in &dup_key {
        prefixes_match(input);
    }
    let count = |o: &str| outcomes.get(o).copied().unwrap_or(0);
    assert!(count("BothAccept") + count("BothReject") > 4 * count("OutOfDomain"), "{outcomes:?}");
    let missing: Vec<_> = SHARE_PATHS.iter().filter(|p| !paths.contains(*p)).collect();
    assert!(missing.is_empty(), "no seed reaches {missing:?}");
    // EV2b D23: share updates and a spend of a shared masternode from an earlier block.
    for path in [PROUPSHARE, PROUPSHAREDREG, BAD_SHARED_COLLATERAL_SPEND] {
        assert!(held.contains(path), "no derived-case seed reaches {path}: {held:?}");
    }
}

/// Every prefix of `input`'s block agrees with Core, so the transaction that fails a dup-key
/// seed is the same on both sides (Opus review r3).
fn prefixes_match(input: &[u8]) {
    let input = generate(input).expect("a generated input");
    let (header, txs) = split_block(&input.block).expect("a block");
    let oracle = Oracle::new(Pin::V24_0);
    for n in 2..=txs.len() {
        let block = assemble(header, &txs[..n]);
        compare_dml_apply(&oracle, &input.chain(), &input.case.params, &input.ctx, &block)
            .unwrap_or_else(|d| panic!("prefix of {n}: {d}"));
    }
}

fn compared(input: &[u8]) -> Outcome {
    compare_apply(input, Pin::V24_0).unwrap_or_else(|d| panic!("{input:02x?}: {d}"))
}

/// A shared registration replacing, by collateral, the only masternode holding one of its share
/// owner keys: Core removes that masternode first (`specialtxman.cpp` v24:389-401), so the share
/// owner check (v24:417-419) does not fail, and the block's `bad-protx-dup-key` comes from a later
/// check (Opus review r3). Core accepts the same registrations when nothing else is held.
#[test]
fn a_replaced_masternode_does_not_hold_share_keys() {
    for hex in [
        "0203035bc166e4dfaed5aa99d007ebbef3b40fbb08349227",
        "050303ccc74eea19d54e864d634da1747ec87b",
    ] {
        let input = unhex(hex).expect("hex");
        assert_eq!(compared(&input), Outcome::BothReject, "{hex}");
        assert!(!share_paths(&input).contains(&SHARE_OWNER_DUP_KEY), "{hex}");
    }
    for hex in [
        "0b0303325e96e1187c36906dcfbc99",
        "030303bb102a1e5c18a651e594a784597199909d9661332f213b477f",
        "000303bc7f3915fdc29d5a3a3878cc45",
    ] {
        assert_eq!(compared(&unhex(hex).expect("hex")), Outcome::BothAccept, "{hex}");
    }
}

/// Whether `input` starts from the derived shared case (EV2b D23) and its block registers no
/// shared masternode, so a share path it reaches is the list's shared masternode's.
fn from_held_shared_mn(input: &[u8]) -> bool {
    generate(input).is_some_and(|input| {
        input.case.prior.is_some()
            && !input.txs().iter().any(|tx| {
                matches!(
                    payload(tx),
                    Some(SpecialPayload::ProviderRegister(p)) if !p.shares.is_empty()
                )
            })
    })
}

/// EV2b D23: the derived case's list is the one Core builds from its prior block (a shared
/// ProReg on a crafted case with V24 forced on) with that block's hash set, it holds a shared
/// masternode, and the case's own coinbase-only block builds on both sides.
#[test]
fn the_shared_case_is_cores() {
    let oracle = Oracle::new(Pin::V24_0);
    let case = cases().iter().find(|c| c.prior.is_some()).expect("the derived case");
    let prior = case.prior.as_ref().expect("a prior block");
    let chain = case.chain();
    let core = oracle.dml_apply(&chain, &prior.block, &prior.ctx).expect("Core's build");
    let CoreList::Supplied(list) = &case.ctx.mn_list else {
        panic!("the derived case supplies its list");
    };
    let mut core_list = core.new_list.expect("Core's list");
    // The prior block's hash: the case's own block's `hashPrevBlock`.
    core_list[..32].copy_from_slice(&case.block[4..36]);
    assert_eq!(&core_list, list);
    let ours = core_disk::decode_list_s3(&mut &list[..]).expect("our read of Core's list");
    assert!(ours.iter().any(|d| d.cold.is_shared()));
    let outcome = compare_dml_apply(&oracle, &chain, &case.params, &case.ctx, &case.block)
        .unwrap_or_else(|d| panic!("{d}"));
    assert_eq!(outcome, Outcome::BothAccept);
}

/// EV2b D25: the quirk vectors `dash-dml/tests/quirks_ev2b.rs` replays without the oracle are
/// still Core's results.
#[test]
fn the_quirk_vectors_are_cores() {
    let vectors = read_vectors(include_str!("../../dash-dml/tests/data/ev2b-quirks.txt"))
        .expect("the vector file");
    assert!(vectors.len() >= 7);
    let oracle = Oracle::new(Pin::V24_0);
    for v in &vectors {
        let (chain, ctx) = oracle_context(v).expect("the oracle fields");
        assert_eq!(
            core_verdict(&oracle, &chain, &ctx, &v.block),
            Some((v.core_payee, v.core.clone())),
            "{}: Core's result changed",
            v.name
        );
    }
}
