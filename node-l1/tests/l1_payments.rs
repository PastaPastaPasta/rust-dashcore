//! EV3-1's L0 and L1 evidence for `diff_block_value_payee` (`node_l1::payments`): every mined
//! sample of O2a-GV's payments fixture under every node state, one crafted case per Core reject
//! string and leniency (R4 F3, F6; quirk 14; MNO-12), V24's strict multiplicity and superblock
//! duplicates, edits of the payee's payout forms, the fuzz target's public seeds and a proptest
//! twin of it.
//!
//! Every case must agree with Core field by field; the asserts on Core's record pin down which
//! branch each case reached.

use bitcoin_hashes::sha256;
use dash_dml::testkit::replay::payee_rules;
use dash_oracle_sys::{BlockValuePayee, Oracle, Pin, ThresholdState, Verdict};
use node_l1::payments::{
    Case, MAX_OPS, Payments, compare_block_value_payee, compare_case_record, sample_index, samples,
    seed_inputs, superblock_case, triggers,
};
use node_l1::{Outcome, target};
use proptest::prelude::*;

fn index(name: &str) -> usize {
    sample_index(name).unwrap_or_else(|| panic!("{name}"))
}

fn case(name: &str) -> Case {
    Case::mined(index(name)).unwrap()
}

fn synced(mut case: Case) -> Case {
    case.pay.synced = true;
    case
}

/// Both sides agree on `case`; Core's record.
fn agree(case: &Case) -> (Outcome, BlockValuePayee) {
    match compare_case_record(case) {
        Ok((outcome, Some(core))) => (outcome, core),
        Ok((outcome, None)) => panic!("no comparison: {outcome:?}"),
        Err(d) => panic!("divergence: {d}"),
    }
}

fn accepted(case: &Case) -> BlockValuePayee {
    let (outcome, core) = agree(case);
    assert_eq!(outcome, Outcome::BothAccept, "{core:?}");
    core
}

fn rejected(case: &Case, reason: &str) -> BlockValuePayee {
    let (outcome, core) = agree(case);
    assert_eq!(outcome, Outcome::BothReject, "{core:?}");
    match &core.verdict {
        Verdict::Reject(rej) => assert_eq!(rej.reason, reason, "{core:?}"),
        Verdict::Accept => unreachable!(),
    }
    core
}

/// `bad-cb-amount` with Core's error naming `why`.
fn bad_amount(case: &Case, why: &str) {
    let core = rejected(case, "bad-cb-amount");
    assert!(core.value_error.contains(why), "{:?} lacks {why:?}", core.value_error);
}

fn value_out(case: &Case) -> i64 {
    case.coinbase.outputs.iter().map(|o| o.value).sum()
}

/// `case` with output `at` changed so that the coinbase pays `total`.
fn paying_at(mut case: Case, at: usize, total: i64) -> Case {
    let delta = total - value_out(&case);
    case.coinbase.outputs[at].value += delta;
    case
}

/// The reward a sample's block may pay: its subsidy (Core's) plus its fees.
fn reward_of(case: &Case) -> i64 {
    agree(case).1.block_subsidy + case.pay.fee_reward
}

/// Every sample under every node state: initial sync, synced, ChainLocked at its height and just
/// below, governance not loaded, V24 seeded active, with and without the trigger at the samples
/// that have one (`first-superblock`, before DIP3, has none), and without the list.
#[test]
fn every_sample_under_every_state() {
    let mut counts = [0usize; 3];
    for (i, sample) in samples().iter().enumerate() {
        for flags in 0..64u8 {
            if flags & 16 != 0 && triggers()[i].is_none() {
                continue;
            }
            for chainlock in [1, 2] {
                if flags & 4 == 0 && chainlock == 2 {
                    continue;
                }
                let input = [i as u8, flags, chainlock, 0, 0];
                let case = node_l1::payments::case_from_bytes(&input).unwrap().unwrap();
                match compare_case_record(&case) {
                    Ok((Outcome::BothAccept, _)) => counts[0] += 1,
                    Ok((Outcome::BothReject, _)) => {
                        // Only a synced check of what a mined block lacks (the budget window's
                        // budget, a superblock's trigger), or the strict sample, whose list the
                        // fixture gives an operator reward the mined coinbase does not pay.
                        let synced_lack = [
                            "budget-window",
                            "superblock",
                            "first-superblock",
                            "pre-v20-superblock",
                        ]
                        .contains(&sample.name.as_str())
                            && flags & 1 != 0;
                        assert!(
                            synced_lack || sample.name == "strict",
                            "{} flags {flags:#x}",
                            sample.name
                        );
                        counts[1] += 1;
                    }
                    Ok((Outcome::OutOfDomain, _)) => {
                        // Only a context without the list the payee check reads.
                        assert!(flags & 32 != 0 && sample.prev_list.is_some(), "{}", sample.name);
                        counts[2] += 1;
                    }
                    Ok((other, _)) => panic!("{other:?}"),
                    Err(d) => panic!("{} flags {flags:#x}: divergence {d}", sample.name),
                }
            }
        }
    }
    assert_eq!(counts, [660, 56, 388]);
}

/// The regular blocks' strings: the reward bound off superblock heights, and the masternode
/// payment, whose absence or a duff short is `bad-cb-payee`, synced or not (R4 F3: the payee is
/// checked in initial sync too).
#[test]
fn regular_block_strings() {
    let mined = case("regular");
    let reward = reward_of(&mined);
    accepted(&paying_at(case("regular"), 0, reward));
    for sync in [false, true] {
        let with = |c: Case| {
            if sync {
                synced(c)
            } else {
                c
            }
        };
        bad_amount(
            &with(paying_at(case("regular"), 0, reward + 1)),
            "only regular blocks are allowed at this height",
        );
        let core = accepted(&with(case("regular")));
        // Drop each owed output in turn.
        let owed = core.masternode.clone();
        assert!(!owed.is_empty());
        for expected in &owed {
            let mut c = with(case("regular"));
            let at = c
                .coinbase
                .outputs
                .iter()
                .position(|o| {
                    o.value == expected.value
                        && o.script_pubkey.as_bytes() == expected.script_pubkey.as_slice()
                })
                .unwrap();
            c.coinbase.outputs.remove(at);
            rejected(&c, "bad-cb-payee");
            let mut short = with(case("regular"));
            short.coinbase.outputs[at].value -= 1;
            rejected(&short, "bad-cb-payee");
        }
    }
}

/// The old budget strings, and MNO-12 / quirk 14: the budget window's mined block, which paid a
/// budget, is valid only where no superblock validation is required (not synced, or ChainLocked
/// at its height).
#[test]
fn old_budget_strings_and_mno_12() {
    let pre = case("pre-budget");
    let reward = reward_of(&pre);
    accepted(&paying_at(case("pre-budget"), 0, reward));
    bad_amount(&paying_at(case("pre-budget"), 0, reward + 1), "old budgets are not activated yet");

    let window = case("budget-window");
    let reward = reward_of(&window);
    assert!(value_out(&window) > reward, "the window block pays a budget");
    accepted(&window);
    bad_amount(&synced(case("budget-window")), "old budgets are disabled");
    let mut locked = synced(case("budget-window"));
    locked.pay.best_chainlock_height = Some(locked.sample_height());
    accepted(&locked);
    let mut below = synced(case("budget-window"));
    below.pay.best_chainlock_height = Some(below.sample_height() - 1);
    bad_amount(&below, "old budgets are disabled");

    let outside = case("budget-outside");
    let reward = reward_of(&outside);
    accepted(&synced(paying_at(case("budget-outside"), 0, reward)));
    bad_amount(
        &paying_at(case("budget-outside"), 0, reward + 1),
        "block is not in old budget cycle window",
    );
}

/// A superblock sample, synced, with the trigger its coinbase pays (`superblock_case`).
fn superblock(name: &str) -> (Case, Payments) {
    let (c, payments) = superblock_case(index(name)).expect("a superblock sample");
    (synced(c), payments)
}

/// The superblock strings on mainnet's and testnet's superblocks, with the trigger the coinbase
/// pays: valid synced; a payment changed, missing or doubled is an invalid superblock (value
/// check); without a trigger the reward bound; above the payments limit the superblock maximum;
/// ChainLocked or not synced, the maximum only (quirk 14).
#[test]
fn superblock_strings() {
    for name in ["superblock", "testnet-superblock"] {
        let with_trigger = |edit: &dyn Fn(&mut Payments)| {
            let (mut c, mut payments) = superblock(name);
            edit(&mut payments);
            c.trigger = Some(payments);
            c
        };
        let core = accepted(&with_trigger(&|_| {}));
        assert!(core.triggered && !core.superblock.is_empty(), "{name}: {core:?}");

        // A payment a duff larger than the coinbase pays: the trigger asks for it, the coinbase
        // does not have it.
        let more = |p: &mut Payments| p[0].1 += 1;
        bad_amount(&with_trigger(&more), "invalid superblock detected");
        // The first payment repeated last. Each payment's search starts at the output the one
        // before it matched (from V24, after it): on mainnet the repeat lies behind that, on
        // testnet, whose trigger has one payment, it is the same output, which only V24 refuses.
        let repeat = |p: &mut Payments| p.push(p[0].clone());
        if name == "superblock" {
            bad_amount(&with_trigger(&repeat), "invalid superblock detected");
        } else {
            accepted(&with_trigger(&repeat));
            let mut v24 = with_trigger(&repeat);
            v24.v24 = ThresholdState::Active;
            bad_amount(&v24, "invalid superblock detected");
        }

        // Not synced, or ChainLocked: only the maximum (the trigger's payments are not checked).
        let mut ibd = with_trigger(&more);
        ibd.pay.synced = false;
        accepted(&ibd);
        let mut locked = with_trigger(&more);
        locked.pay.best_chainlock_height = Some(locked.sample_height());
        accepted(&locked);

        // No trigger: the coinbase pays more than the reward.
        bad_amount(&superblock(name).0, "no triggered superblock detected");
        let (mut ibd, _) = superblock(name);
        ibd.pay.synced = false;
        accepted(&ibd);

        // A duff over the superblock maximum (the reward plus the payments limit), with the
        // trigger: refused in every state, the bound initial sync and ChainLocks keep.
        let s = superblock(name).0.sample();
        let limit =
            Oracle::new(Pin::V24_0).superblock_payments_limit(&(s.net.chain)(), s.height).unwrap();
        let reward = reward_of(&superblock(name).0);
        for (synced, locked) in [(false, false), (true, false), (true, true)] {
            let mut c = with_trigger(&|_| {});
            c = paying_at(c, 0, reward + limit + 1);
            c.pay.synced = synced;
            if locked {
                c.pay.best_chainlock_height = Some(c.sample_height());
            }
            bad_amount(&c, "exceeded superblock max value");
        }
    }
}

/// V24 (testnet: mainnet's V24 is never active): two equal owed outputs need two coinbase
/// outputs, and two equal superblock payments two outputs.
#[test]
fn v24_strict_multiplicity() {
    // The strict sample's payee pays one script twice; find fees that split the reward evenly.
    let (fee, owed) = (0..64)
        .find_map(|fee| {
            let mut c = case("strict");
            c.pay.fee_reward += fee;
            c.v24 = ThresholdState::Active;
            let mn = agree(&c).1.masternode;
            let n = mn.len();
            (n >= 2 && mn[n - 1] == mn[n - 2]).then_some((fee, mn))
        })
        .expect("an even masternode reward");
    let coinbase = |n: usize| {
        let mut c = case("strict");
        c.pay.fee_reward += fee;
        let reward = reward_of(&c);
        let miner = c.coinbase.outputs[0].clone();
        let mut outs = vec![miner];
        for o in &owed[..n] {
            outs.push(dash_primitives::TxOut {
                value: o.value,
                script_pubkey: o.script_pubkey.clone().into(),
            });
        }
        outs[0].value = reward - owed[..n].iter().map(|o| o.value).sum::<i64>();
        c.coinbase.outputs = outs;
        c
    };
    for sync in [false, true] {
        let one = |v24| {
            let mut c = coinbase(owed.len() - 1);
            c.pay.synced = sync;
            c.v24 = v24;
            c
        };
        accepted(&one(ThresholdState::Defined));
        assert!(rejected(&one(ThresholdState::Active), "bad-cb-payee").strict);
        let mut two = coinbase(owed.len());
        two.pay.synced = sync;
        two.v24 = ThresholdState::Active;
        accepted(&two);
    }
}

/// F6: with every masternode banned there is no payee, and any coinbase passes the payee check.
#[test]
fn no_payee_accepts_any_payee() {
    let core = accepted(&synced(case("banned")));
    assert!(!core.txouts_ok && core.masternode.is_empty());
    let mut nothing = synced(case("banned"));
    nothing.coinbase.outputs.truncate(1);
    accepted(&nothing);
}

/// The MN_RR era (testnet): the platform share is the first owed output, to `OP_RETURN`.
#[test]
fn platform_share_is_owed() {
    let core = accepted(&case("platform"));
    assert_eq!(core.masternode[0].script_pubkey, [0x6a]);
    let mut no_share = case("platform");
    let at = no_share
        .coinbase
        .outputs
        .iter()
        .position(|o| o.script_pubkey.as_bytes() == [0x6a])
        .unwrap();
    no_share.coinbase.outputs.remove(at);
    rejected(&no_share, "bad-cb-payee");
}

/// The payee's payout forms, edited through the fuzz format: operator rewards with and without a
/// script, a version-3 payout list, collateral shares, and a banned payee (the next is paid).
/// Core's outputs show each form: the operator's script last, and payout lists and shares split
/// over several owner outputs.
#[test]
fn payee_payout_forms() {
    let mut accepted_edits = 0;
    let mut owed_forms = std::collections::BTreeSet::new();
    let mut operator_last = 0;
    let mut refused = 0;
    let mut most_owner_outputs = [0usize; 12];
    for name in ["regular", "platform", "strict", "superblock", "testnet-superblock"] {
        let i = index(name) as u8;
        for op in 8..=11u8 {
            for a in 0..6u8 {
                for b in [0u8, 1, 2, 5, 0x42, 0xff] {
                    for flags in [0u8, 1, 8 | 1] {
                        let input = [i, flags, 0, 0, 0, op, a, b];
                        let case = node_l1::payments::case_from_bytes(&input).unwrap().unwrap();
                        let (outcome, core) = match compare_case_record(&case) {
                            Ok((outcome, Some(core))) => (outcome, core),
                            // Payouts above 10000 in total, or shares summing to 0.
                            Ok((Outcome::OutOfDomain, None)) if op == 9 || op == 10 => {
                                refused += 1;
                                continue;
                            }
                            Ok((outcome, None)) => panic!("{name} {input:?}: {outcome:?}"),
                            Err(d) => panic!("{name} {input:?}: divergence {d}"),
                        };
                        // The mined coinbase no longer pays an edited payee.
                        if outcome == Outcome::BothAccept {
                            accepted_edits += 1;
                        }
                        owed_forms.insert((name, op, core.masternode.len()));
                        // An operator reward with a script (`edit_operator`: even `b`), paid last.
                        let operator = [&[0x76, 0xa9, 0x14][..], &[b; 20], &[0x88, 0xac]].concat();
                        if op == 8 && b % 2 == 0 && a != 0 && core.txouts_ok {
                            let last = core.masternode.last().unwrap();
                            assert_eq!(last.script_pubkey, operator, "{name} {input:?}: {core:?}");
                            operator_last += 1;
                        }
                        // The owners' outputs: neither the platform share nor the operator's.
                        let rules = payee_rules(case.sample().net.params, case.sample_height() - 1);
                        let payee = case.list.as_ref().and_then(|list| list.payee(rules));
                        let operator_script = payee.map(|p| p.cold.script_operator_payout.clone());
                        let owner = core.masternode.iter().filter(|o| {
                            o.script_pubkey != [0x6a]
                                && Some(&o.script_pubkey) != operator_script.as_ref()
                        });
                        let n = owner.count();
                        most_owner_outputs[usize::from(op)] =
                            most_owner_outputs[usize::from(op)].max(n);
                    }
                }
            }
        }
    }
    assert!(accepted_edits > 0);
    // Several output counts per edit kind: the edits reach different payout shapes.
    assert!(owed_forms.len() > 20, "{owed_forms:?}");
    assert!(operator_last > 0 && refused > 0);
    // Payout lists (9) and shares (10) of three or more owners.
    assert!(most_owner_outputs[9] >= 3 && most_owner_outputs[10] >= 3, "{most_owner_outputs:?}");
}

/// A payee state Core refuses (payouts above 10000 in total) on a block whose value check fails
/// off a superblock height: the value check needs no governance answer, so it is compared, though
/// the same state paying the reward is out of Core's domain.
#[test]
fn refused_payee_state_after_a_failed_value_check() {
    // `edit_payouts` with `a = 1`, `b = 0xc6`: two payouts of 9999.
    let payouts = [9, 1, 0xc6];
    let mut refused = vec![index("platform") as u8, 1, 0, 0, 0];
    refused.extend(payouts);
    assert_eq!(compare_block_value_payee(&refused, Pin::V24_0), Ok(Outcome::OutOfDomain));
    let mut overpaid = refused.clone();
    overpaid.extend([3, 0, 0]);
    let case = node_l1::payments::case_from_bytes(&overpaid).unwrap().unwrap();
    bad_amount(&case, "only regular blocks are allowed at this height");
}

/// The registered target's start and comparison, as node-fuzz runs them.
#[test]
fn target_is_registered() {
    let t = target("diff_block_value_payee").unwrap();
    (t.start.unwrap())(Pin::V24_0);
    assert_eq!((t.compare)(&[], Pin::V24_0), Ok(Outcome::OutOfDomain));
    assert!((t.compare)(&[3, 1, 0, 0, 0], Pin::V24_0).is_ok());
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        ..ProptestConfig::default()
    })]

    /// The fuzz target's twin: random inputs never diverge.
    #[test]
    fn random_inputs_agree(
        head in prop::array::uniform5(any::<u8>()),
        ops in prop::collection::vec(any::<u8>(), 0..3 * MAX_OPS),
    ) {
        let mut input = head.to_vec();
        input.extend(ops);
        let compared = (target("diff_block_value_payee").unwrap().compare)(&input, Pin::V24_0);
        prop_assert!(compared.is_ok(), "{input:?}: {compared:?}");
    }
}

/// `diff_block_value_payee` agrees with Core on each of its public seeds (`node-fuzz/public-seeds`,
/// written by the `payments_seeds` example), and each seed is current.
#[test]
fn fuzz_target_agrees_on_its_public_seeds() {
    let dir =
        concat!(env!("CARGO_MANIFEST_DIR"), "/../node-fuzz/public-seeds/diff_block_value_payee");
    let mut names = std::collections::BTreeSet::new();
    for input in seed_inputs() {
        let digest = sha256::Hash::hash(&input).to_byte_array();
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        let path = format!("{dir}/{name}");
        assert_eq!(std::fs::read(&path).ok(), Some(input.clone()), "{path}: rerun payments_seeds");
        if let Err(d) = compare_block_value_payee(&input, Pin::V24_0) {
            panic!("{input:?}: divergence {d}");
        }
        names.insert(name);
    }
    let on_disk = std::fs::read_dir(dir).expect("the seed directory").count();
    assert_eq!(on_disk, names.len(), "stale seeds in {dir}: rerun payments_seeds");
}
