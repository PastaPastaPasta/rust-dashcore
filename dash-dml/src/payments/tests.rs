//! `GetBlockTxOuts`' output assembly and `IsTransactionValid` (EV3-1): the owner part over each
//! payout form, the operator part, the platform share, and the lists with no payee. The
//! amounts come from `dash_consensus::payments` (CS2's tests cover its arithmetic); Core's own
//! answers on mined lists are node-l1's `diff_block_value_payee`.

use std::sync::Arc;

use dash_consensus::subsidy::COIN;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{CollateralShare, NetInfo, NetInfoV2, PayoutShare, Transaction, TxType};

use super::*;
use crate::testkit::dmn_with;
use crate::{Dmn, MnCold, MnType};

const RULES: PayeeRules = PayeeRules {
    v19: true,
    mn_rr: true,
};

/// A height past every mainnet reallocation step, where the eras differ.
const HEIGHT: i32 = 2_200_000;

fn params() -> PaymentParams {
    let chain = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0)
        .build()
        .expect("Core's parameters");
    PaymentParams::from_chain_params(&chain).expect("payment parameters")
}

fn reward(era: MnRewardEra) -> BlockReward {
    BlockReward {
        height: HEIGHT,
        block_subsidy: 2 * COIN + 12_345,
        fee_reward: 77_777,
        era,
    }
}

fn script(tag: u8) -> Vec<u8> {
    vec![0x76, 0xa9, tag]
}

fn out(value: Amount, tag: u8) -> TxOut {
    TxOut {
        value,
        script_pubkey: script(tag).into(),
    }
}

/// The list at `HEIGHT - 1` with `mns`.
fn list(mns: impl IntoIterator<Item = Arc<Dmn>>) -> MnList {
    let mut list = MnList::default_pre_dip3().with_height(HEIGHT - 1);
    for dmn in mns {
        list.try_add(dmn, true).expect("distinct masternodes");
    }
    list
}

/// One masternode, the payee, with payout script `script(1)` and `f`'s changes.
fn payee(f: impl FnOnce(&mut Dmn, &mut MnCold)) -> MnList {
    list([dmn_with(1, MnType::Regular, |dmn, cold| {
        cold.script_payout = script(1);
        f(dmn, cold);
    })])
}

/// `cold` as a version-3 (ExtAddr) state, with an empty extended NetInfo.
fn to_ext_addr(cold: &mut MnCold) {
    cold.version = 3;
    cold.script_payout = Vec::new();
    cold.net_info = Arc::new(NetInfo::Extended(NetInfoV2 {
        version: 1,
        entries: Vec::new(),
    }));
}

/// `GetBlockTxOuts`' outputs where it returns true.
fn outs(list: &MnList, era: MnRewardEra) -> Option<Vec<TxOut>> {
    let owed = block_tx_outs(list, RULES, &reward(era), &params()).expect("Core answers");
    owed.computed.then_some(owed.outputs)
}

/// The masternodes' part of the block reward in `era`, after the platform share.
fn masternode_reward(era: MnRewardEra) -> Amount {
    let (r, p) = (reward(era), params());
    let all = masternode_payment(HEIGHT, r.block_subsidy + r.fee_reward, era, &p);
    match era {
        MnRewardEra::EvoReward => {
            all - platform_share(masternode_payment(HEIGHT, r.block_subsidy, era, &p)).unwrap()
        }
        _ => all,
    }
}

fn coinbase(outputs: Vec<TxOut>) -> Tx {
    Tx::new(Transaction {
        version: 3,
        tx_type: TxType::new(5),
        inputs: vec![],
        outputs,
        lock_time: 0,
        extra_payload: vec![],
    })
}

// Core: masternode/payments.cpp:142-156
#[test]
fn empty_list_owes_only_the_platform_share() {
    let empty = list([]);
    assert_eq!(outs(&empty, MnRewardEra::CreditPool), Some(vec![]));
    assert_eq!(outs(&empty, MnRewardEra::Classic), Some(vec![]));
    let p = params();
    let platform = platform_share(masternode_payment(
        HEIGHT,
        reward(MnRewardEra::EvoReward).block_subsidy,
        MnRewardEra::EvoReward,
        &p,
    ))
    .unwrap();
    assert_eq!(
        outs(&empty, MnRewardEra::EvoReward),
        Some(vec![TxOut {
            value: platform,
            script_pubkey: vec![OP_RETURN].into(),
        }])
    );
}

/// Every masternode banned: matches Core (possible security note: see manager).
#[test]
fn no_payable_masternode_matches_core() {
    let banned = payee(|dmn, _| dmn.hot.ban_if_not_banned(HEIGHT - 10));
    for era in [MnRewardEra::CreditPool, MnRewardEra::EvoReward] {
        let owed = block_tx_outs(&banned, RULES, &reward(era), &params()).unwrap();
        let platform = outs(&list([]), era).unwrap();
        assert_eq!(platform.len(), usize::from(era == MnRewardEra::EvoReward));
        assert_eq!(
            owed,
            BlockTxOuts {
                outputs: platform,
                computed: false
            }
        );
        for strict in [false, true] {
            let valid = masternode_payee_valid(
                &coinbase(vec![]),
                &banned,
                RULES,
                &reward(era),
                strict,
                &params(),
            );
            assert_eq!(valid, Ok(true));
        }
    }
}

// Core: masternode/payments.cpp:162-198
#[test]
fn legacy_payee_and_operator() {
    for era in [MnRewardEra::Classic, MnRewardEra::CreditPool, MnRewardEra::EvoReward] {
        let mn = masternode_reward(era);
        let platform: Vec<TxOut> = outs(&list([]), era).expect("an empty list has an answer");
        let with = |tail: Vec<TxOut>| Some(platform.iter().cloned().chain(tail).collect());

        // A legacy payout script gets the whole reward.
        assert_eq!(outs(&payee(|_, _| {}), era), with(vec![out(mn, 1)]));

        // An operator reward with a payout script: its part comes last.
        let op = payee(|dmn, cold| {
            dmn.operator_reward = 1_234;
            cold.script_operator_payout = script(2);
        });
        let op_part = mn * 1_234 / 10_000;
        assert_eq!(outs(&op, era), with(vec![out(mn - op_part, 1), out(op_part, 2)]));

        // Without an operator payout script, or with a 0 reward, the owner gets it all.
        let no_script = payee(|dmn, _| dmn.operator_reward = 1_234);
        assert_eq!(outs(&no_script, era), with(vec![out(mn, 1)]));
        let no_reward = payee(|_, cold| cold.script_operator_payout = script(2));
        assert_eq!(outs(&no_reward, era), with(vec![out(mn, 1)]));

        // The whole reward to the operator: no owner output of 0.
        let all_op = payee(|dmn, cold| {
            dmn.operator_reward = 10_000;
            cold.script_operator_payout = script(2);
        });
        assert_eq!(outs(&all_op, era), with(vec![out(mn, 2)]));
    }
}

/// Version 3 (ExtAddr): the payout list, each entry its share rounded down and the last the
/// remainder; entries of 0 are left out. An empty list pays the owner nothing.
// Core: masternode/payments.cpp:181-193; evo/providertx.h:94 GetOwnerPayouts
#[test]
fn ext_addr_payout_list() {
    let era = MnRewardEra::CreditPool;
    let mn = masternode_reward(era);
    let shares = |list: &[(u8, u16)]| -> Vec<PayoutShare> {
        list.iter()
            .map(|&(tag, reward)| PayoutShare {
                script_payout: script(tag).into(),
                reward,
            })
            .collect()
    };
    let v3 = |payouts: Vec<PayoutShare>| {
        payee(move |dmn, cold| {
            to_ext_addr(cold);
            cold.payouts = payouts;
            dmn.operator_reward = 500;
            cold.script_operator_payout = script(9);
        })
    };
    let op_part = mn * 500 / 10_000;
    let owner = mn - op_part;
    let first = owner * 3_333 / 10_000;
    assert_eq!(
        outs(&v3(shares(&[(3, 3_333), (4, 6_667)])), era),
        Some(vec![out(first, 3), out(owner - first, 4), out(op_part, 9)])
    );
    // An entry of 0 basis points has no output; the last still gets the remainder.
    assert_eq!(
        outs(&v3(shares(&[(3, 0), (4, 10_000)])), era),
        Some(vec![out(owner, 4), out(op_part, 9)])
    );
    // The last entry gets the remainder, whatever its own basis points.
    let half = owner * 5_000 / 10_000;
    assert_eq!(
        outs(&v3(shares(&[(3, 5_000), (4, 1)])), era),
        Some(vec![out(half, 3), out(owner - half, 4), out(op_part, 9)])
    );
    // Version 3 reads the list, not the legacy script.
    assert_eq!(outs(&v3(vec![]), era), Some(vec![out(op_part, 9)]));
}

/// A shared masternode: the owner part split by collateral, to each share's reward script, or
/// its refund script when it has none.
// Core: masternode/payments.cpp:171-180; evo/providertx.h:80 CCollateralShare::RewardScript
#[test]
fn shared_masternode_split() {
    let era = MnRewardEra::EvoReward;
    let mn = masternode_reward(era);
    let share = |amount: Amount, refund: u8, reward: Option<u8>| CollateralShare {
        amount,
        script_refund: script(refund).into(),
        script_reward: reward.map(script).unwrap_or_default().into(),
        key_id_owner: [refund; 20].into(),
    };
    let shared = payee(|_, cold| {
        to_ext_addr(cold);
        cold.shares = vec![
            share(600 * COIN, 10, Some(20)),
            share(300 * COIN, 11, None),
            share(100 * COIN, 12, Some(22)),
        ];
    });
    let split = split_amount_by_shares(mn, &[600 * COIN, 300 * COIN, 100 * COIN]).unwrap();
    let platform = outs(&list([]), era).unwrap();
    let mut expected = platform.clone();
    expected.extend([out(split[0], 20), out(split[1], 11), out(split[2], 22)]);
    assert_eq!(outs(&shared, era), Some(expected));

    // A share whose part rounds to 0 has no output.
    let tiny = payee(|_, cold| {
        to_ext_addr(cold);
        cold.shares = vec![share(1, 10, None), share(1_000 * COIN, 11, None)];
    });
    let mut expected = platform;
    expected.push(out(mn, 11));
    assert_eq!(outs(&tiny, era), Some(expected));

    // Shares that sum to 0: Core's division throws.
    let zero = payee(|_, cold| {
        to_ext_addr(cold);
        cold.shares = vec![share(0, 10, None), share(0, 11, None)];
    });
    assert_eq!(
        block_tx_outs(&zero, RULES, &reward(era), &params()),
        Err(PayeeError::Payment(PaymentError::ZeroShareTotal))
    );
}

/// The payee is the list's (`GetMNPayee`), not its first masternode.
#[test]
fn pays_the_lists_payee() {
    let paid_recently = |n: u8, paid: i32| {
        dmn_with(n, MnType::Regular, move |dmn, cold| {
            cold.script_payout = script(n);
            dmn.hot.last_paid_height = paid;
        })
    };
    let two = list([paid_recently(1, HEIGHT - 2), paid_recently(2, HEIGHT - 5)]);
    let mn = masternode_reward(MnRewardEra::CreditPool);
    assert_eq!(outs(&two, MnRewardEra::CreditPool), Some(vec![out(mn, 2)]));
}

// Core: masternode/payments.cpp:227-260 IsTransactionValid
#[test]
fn coinbase_must_pay_every_owed_output() {
    let era = MnRewardEra::EvoReward;
    let p = params();
    let op = payee(|dmn, cold| {
        dmn.operator_reward = 1_000;
        cold.script_operator_payout = script(2);
    });
    let owed = outs(&op, era).unwrap();
    assert_eq!(owed.len(), 3, "platform, owner, operator");
    let valid = |outputs: Vec<TxOut>, strict: bool| {
        masternode_payee_valid(&coinbase(outputs), &op, RULES, &reward(era), strict, &p).unwrap()
    };
    let miner = out(5 * COIN, 7);
    for strict in [false, true] {
        let mut all = vec![miner.clone()];
        all.extend(owed.iter().rev().cloned());
        assert!(valid(all, strict));
        for skip in 0..owed.len() {
            let mut missing = vec![miner.clone()];
            missing.extend(
                owed.iter().enumerate().filter(|(i, _)| *i != skip).map(|(_, o)| o.clone()),
            );
            assert!(!valid(missing, strict), "output {skip} owed");
        }
        let mut short = owed.clone();
        short[1].value -= 1;
        assert!(!valid(short, strict));
    }

    // An owner payout list naming one script twice owes two equal outputs: one output pays
    // both only before V24.
    let twice = payee(|_, cold| {
        to_ext_addr(cold);
        cold.payouts = vec![
            PayoutShare {
                script_payout: script(3).into(),
                reward: 5_000,
            },
            PayoutShare {
                script_payout: script(3).into(),
                reward: 5_000,
            },
        ];
    });
    // Fees that make the masternode reward even, so the two halves are equal.
    let reward = (0..4)
        .map(|fee_reward| BlockReward {
            fee_reward,
            ..reward(MnRewardEra::CreditPool)
        })
        .find(|r| masternode_payment(r.height, r.block_subsidy + r.fee_reward, r.era, &p) % 2 == 0)
        .unwrap();
    let owed = block_tx_outs(&twice, RULES, &reward, &p).unwrap().outputs;
    assert_eq!(owed.len(), 2);
    assert_eq!(owed[0], owed[1]);
    let one = coinbase(vec![owed[0].clone()]);
    assert_eq!(masternode_payee_valid(&one, &twice, RULES, &reward, false, &p), Ok(true));
    assert_eq!(masternode_payee_valid(&one, &twice, RULES, &reward, true, &p), Ok(false));
    let both = coinbase(owed.clone());
    assert_eq!(masternode_payee_valid(&both, &twice, RULES, &reward, true, &p), Ok(true));
}
