//! `oracle_block_value_payee` and `oracle_credit_pool_step` (ABI 18): crafted rejects for each
//! of Core's coinbase strings, the sync leniencies, v24's strict multiplicity, and credit-pool
//! steps, on mined blocks.
//!
//! Provenance: `tests/data/payments-v24.0.json.gz`, written by
//! `scripts/gen-payments-fixture.py` from the reference dashd v24.0.0 nodes' RPC (mainnet at
//! heights ≤ 2,090,000 and testnet; see PROVENANCE.md). Each expected reason is the string
//! Core's own code writes at the pin: `ConnectBlock()` (validation.cpp:2846-2865) rejects with
//! `bad-cb-amount` then `bad-cb-payee`; `IsBlockValueValid()` and
//! `IsOldBudgetBlockValueValid()` (masternode/payments.cpp) give the value errors;
//! `CheckCreditPoolDiffForBlock()` (evo/specialtxman.cpp) and `CCreditPoolDiff`
//! (evo/creditpool.cpp, rc.4:297-349) the credit-pool ones. The live comparison against every
//! sampled mainnet coinbase is the `payments_check` example (`scripts/payments-check.py`).

mod common;

use std::fmt::Debug;
use std::io::Read;
use std::sync::OnceLock;

use common::hex;
use dash_oracle_sys::payments::{
    VOTE_OUTCOME_ABSTAIN, VOTE_OUTCOME_NO, VOTE_OUTCOME_YES, VOTE_SIGNAL_FUNDING,
};
use dash_oracle_sys::{
    BlockValuePayee, Chain, Checkpoints, CoinsView, Context, CreditPool, CreditPoolStep,
    DEPLOYMENT_V24, Error, ErrorKind, GovVote, MnList, Oracle, PaymentsContext, Pin, ResultCode,
    RewardEra, StepContext, SuperblockCheck, ThresholdState, Trigger, TxOut, UnlockIndexes,
    Verdict,
};
use dashcore::Block;
use dashcore::blockdata::transaction::special_transaction::TransactionType;
use dashcore::consensus::{deserialize, serialize};
use dashcore::hashes::{Hash, sha256d};
use serde_json::Value;

/// `MAX_MONEY` (consensus/amount.h).
const MAX_MONEY: i64 = 21_000_000 * 100_000_000;

/// `BlockValidationResult::BLOCK_RESULT_UNSET` (the payment rejects) and `BLOCK_CONSENSUS`.
const BLOCK_RESULT_UNSET: ResultCode = ResultCode::Block(0);
const BLOCK_CONSENSUS: ResultCode = ResultCode::Block(1);

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

fn fixture() -> &'static Value {
    static FIXTURE: OnceLock<Value> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/payments-v24.0.json.gz");
        let mut text = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(path).expect(path))
            .read_to_string(&mut text)
            .expect(path);
        serde_json::from_str(&text).expect(path)
    })
}

fn named<'a>(list: &'a str, name: &str) -> &'a Value {
    fixture()[list]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap_or_else(|| panic!("{name}"))
}

/// A payments sample: the chain, the block, and its context under deployment states.
struct Sample {
    chain: Chain,
    height: i32,
    block: Block,
    fee: i64,
    header: [u8; 80],
    list: MnList,
    adjusted_time: i64,
    bls_legacy: bool,
}

impl Sample {
    fn load(name: &str) -> Sample {
        let s = named("payments", name);
        Sample {
            chain: Chain::new(s["chain"].as_str().unwrap(), &[]),
            height: s["height"].as_i64().unwrap() as i32,
            block: deserialize(&hex(s["block"].as_str().unwrap())).unwrap(),
            fee: s["fee"].as_i64().unwrap(),
            header: hex(s["tip_header"].as_str().unwrap()).try_into().unwrap(),
            list: match s["prev_list"].as_str() {
                Some(list) => MnList::Supplied(hex(list)),
                None => MnList::Absent,
            },
            adjusted_time: s["adjusted_time"].as_i64().unwrap(),
            bls_legacy: s["bls_legacy"].as_bool().unwrap(),
        }
    }

    fn context(&self, v24: ThresholdState) -> Context {
        Context::new(
            self.height - 1,
            vec![self.header],
            self.bls_legacy,
            vec![(DEPLOYMENT_V24, v24)],
            self.adjusted_time,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            self.list.clone(),
            vec![],
        )
    }

    /// The entry on `block` under `pay`, V24 DEFINED (as on both reference nodes).
    fn run(&self, block: &Block, pay: &PaymentsContext) -> BlockValuePayee {
        self.run_v24(block, pay, ThresholdState::Defined)
    }

    fn run_v24(
        &self,
        block: &Block,
        pay: &PaymentsContext,
        v24: ThresholdState,
    ) -> BlockValuePayee {
        self.try_run(block, pay, v24).unwrap()
    }

    fn try_run(
        &self,
        block: &Block,
        pay: &PaymentsContext,
        v24: ThresholdState,
    ) -> Result<BlockValuePayee, Error> {
        oracle().block_value_payee(&self.chain, &serialize(block), &self.context(v24), pay)
    }

    /// Not synced, governance loaded, no triggers: a node in initial sync.
    fn ibd(&self) -> PaymentsContext {
        PaymentsContext {
            fee_reward: self.fee,
            synced: false,
            best_chainlock_height: None,
            governance_loaded: true,
            triggers: vec![],
        }
    }

    fn synced(&self) -> PaymentsContext {
        PaymentsContext {
            synced: true,
            ..self.ibd()
        }
    }

    fn value_out(block: &Block) -> i64 {
        block.txdata[0].output.iter().map(|o| o.value as i64).sum()
    }

    /// The block with output 0 changed so that the coinbase pays `total`.
    fn paying(&self, total: i64) -> Block {
        Self::paying_at(&self.block, 0, total)
    }

    /// `block` with output `at` changed so that the coinbase pays `total`.
    fn paying_at(block: &Block, at: usize, total: i64) -> Block {
        let mut block = block.clone();
        let value = block.txdata[0].output[at].value as i64 + total - Self::value_out(&block);
        block.txdata[0].output[at].value = u64::try_from(value).expect("a non-negative output");
        block
    }
}

fn outs(block: &Block) -> Vec<TxOut> {
    block.txdata[0]
        .output
        .iter()
        .map(|o| TxOut {
            value: o.value as i64,
            script_pubkey: o.script_pubkey.to_bytes(),
        })
        .collect()
}

/// `verdict` is a rejection with `reason` and `result`.
fn assert_rejected(verdict: &Verdict, reason: &str, result: ResultCode, record: &dyn Debug) {
    match verdict {
        Verdict::Reject(rej) => {
            assert_eq!(rej.reason, reason, "{record:?}");
            assert_eq!(rej.result, result);
        }
        Verdict::Accept => panic!("accepted, expected {reason}: {record:?}"),
    }
}

fn reject(r: &BlockValuePayee, reason: &str) {
    assert_rejected(&r.verdict, reason, BLOCK_RESULT_UNSET, r);
}

/// `bad-cb-amount` with Core's `strErrorRet` naming `why`.
/// ConnectBlock() stops there: no payee check and no outputs.
fn reject_value(r: &BlockValuePayee, why: &str) {
    reject(r, "bad-cb-amount");
    assert!(!r.value_valid && r.payee_valid.is_none() && !r.outputs_computed, "{r:?}");
    assert!(r.value_error.contains(why), "{:?} lacks {why:?}", r.value_error);
}

fn accept(r: &BlockValuePayee) {
    assert!(r.verdict.is_accept(), "{r:?}");
}

/// Position of each expected output in the coinbase, one distinct output each.
fn matched(coinbase: &[TxOut], expected: &[TxOut]) -> Option<Vec<usize>> {
    let mut used = vec![];
    for e in expected {
        used.push((0..coinbase.len()).find(|i| !used.contains(i) && coinbase[*i] == *e)?);
    }
    Some(used)
}

#[test]
fn regular_block_masternode_payment() {
    let s = Sample::load("regular");
    let r = s.run(&s.block, &s.ibd());
    accept(&r);
    assert_eq!(r.check, SuperblockCheck::NoCheck);
    assert!(r.txouts_ok && !r.masternode.is_empty());
    let coinbase = outs(&s.block);
    let found =
        matched(&coinbase, &r.masternode).expect("the masternode payment is in the coinbase");
    let reward = r.block_subsidy + s.fee;

    let r = s.run(&s.block, &s.synced());
    accept(&r);
    assert_eq!(r.check, SuperblockCheck::AllowDuplicates);
    // Mainnet's V24 is NEVER_ACTIVE at the pin: a seeded ACTIVE state changes nothing (D10).
    let r = s.run_v24(&s.block, &s.synced(), ThresholdState::Active);
    assert!(!r.strict);
    assert_eq!(r.check, SuperblockCheck::AllowDuplicates);
    assert!(!r.triggered);
    // Chainlocked at its height, the block needs no superblock validation.
    let locked = PaymentsContext {
        best_chainlock_height: Some(s.height),
        ..s.synced()
    };
    assert_eq!(s.run(&s.block, &locked).check, SuperblockCheck::NoCheck);

    // bad-cb-payee: the masternode output removed (the miner keeps the reward).
    let mut no_payee = s.block.clone();
    no_payee.txdata[0].output.remove(found[0]);
    for pay in [s.ibd(), s.synced()] {
        let r = s.run(&no_payee, &pay);
        reject(&r, "bad-cb-payee");
        assert!(r.value_valid && r.payee_valid == Some(false) && r.outputs_computed);
    }
    // One duff short of the payment is no payment either.
    let mut short = s.block.clone();
    short.txdata[0].output[found[0]].value -= 1;
    reject(&s.run(&short, &s.ibd()), "bad-cb-payee");

    // Off superblock heights the coinbase may pay the reward, not a duff more, synced or not.
    accept(&s.run(&s.paying(reward), &s.ibd()));
    for pay in [s.ibd(), s.synced()] {
        reject_value(
            &s.run(&s.paying(reward + 1), &pay),
            "only regular blocks are allowed at this height",
        );
    }
    // The value check comes first: an overpaying block without its payee is bad-cb-amount.
    assert_ne!(found[0], 0, "output 0 is the miner's");
    let overpay = Sample::paying_at(&no_payee, 0, reward + 1);
    reject_value(&s.run(&overpay, &s.ibd()), "only regular blocks are allowed at this height");
    // ConnectBlock() returns there (validation.cpp:2846-2849), before any read of the list: a
    // context without one gets the same verdict, where the payee check needs it.
    let no_list = Sample {
        list: MnList::Absent,
        ..Sample::load("regular")
    };
    for pay in [no_list.ibd(), no_list.synced()] {
        reject_value(
            &no_list.run(&overpay, &pay),
            "only regular blocks are allowed at this height",
        );
    }
    let e = no_list.try_run(&s.block, &s.ibd(), ThresholdState::Defined).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Context, "{e:?}");
}

#[test]
fn old_budget_and_pre_budget_strings() {
    // Before nBudgetPaymentsStartBlock: the block reward.
    let s = Sample::load("pre-budget");
    let reward = s.run(&s.block, &s.ibd()).block_subsidy + s.fee;
    accept(&s.run(&s.paying(reward), &s.ibd()));
    for pay in [s.ibd(), s.synced()] {
        reject_value(&s.run(&s.paying(reward + 1), &pay), "old budgets are not activated yet");
    }

    // In a budget window the mined block paid the budget (MNO-12): only a node that needs no
    // superblock validation (not synced, or chainlocked) accepts it.
    let s = Sample::load("budget-window");
    let r = s.run(&s.block, &s.ibd());
    accept(&r);
    let reward = r.block_subsidy + s.fee;
    assert!(Sample::value_out(&s.block) > reward, "the window block pays a budget");
    reject_value(&s.run(&s.block, &s.synced()), "old budgets are disabled");
    let locked = PaymentsContext {
        best_chainlock_height: Some(s.height),
        ..s.synced()
    };
    accept(&s.run(&s.block, &locked));
    // The budget payment cut to the reward (the largest output: the miner's may be smaller than
    // the excess).
    let coinbase = outs(&s.block);
    let largest = (0..coinbase.len()).max_by_key(|&i| coinbase[i].value).unwrap();
    accept(&s.run(&Sample::paying_at(&s.block, largest, reward), &s.synced()));

    // Past the window: the block reward, synced or not.
    let s = Sample::load("budget-outside");
    let reward = s.run(&s.block, &s.ibd()).block_subsidy + s.fee;
    accept(&s.run(&s.paying(reward), &s.synced()));
    for pay in [s.ibd(), s.synced()] {
        reject_value(
            &s.run(&s.paying(reward + 1), &pay),
            "block is not in old budget cycle window",
        );
    }
}

/// The superblock sample's trigger: its payments (every coinbase output but the miner's and
/// the masternode payment, in coinbase order) funded by the sample's voters.
fn superblock_trigger(
    s: &Sample,
    sample: &Value,
) -> (Vec<(String, i64)>, Vec<GovVote>, Vec<usize>) {
    let r = s.run(&s.block, &s.ibd());
    let coinbase = outs(&s.block);
    let mn = matched(&coinbase, &r.masternode).unwrap();
    let miner_value = r.block_subsidy + s.fee - mn.iter().map(|&i| coinbase[i].value).sum::<i64>();
    let miner = (0..coinbase.len())
        .find(|&i| coinbase[i].value == miner_value && !mn.contains(&i))
        .unwrap();
    let indexes: Vec<usize> =
        (0..coinbase.len()).filter(|i| *i != miner && !mn.contains(i)).collect();
    let addresses = sample["addresses"].as_array().unwrap();
    let payments = indexes
        .iter()
        .map(|&i| (addresses[i].as_str().unwrap().to_string(), coinbase[i].value))
        .collect();
    let voters = sample["voters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| GovVote {
            collateral_txid: hex(v[0].as_str().unwrap()).try_into().unwrap(),
            collateral_vout: v[1].as_u64().unwrap() as u32,
            signal: VOTE_SIGNAL_FUNDING,
            outcome: VOTE_OUTCOME_YES,
        })
        .collect();
    (payments, voters, indexes)
}

#[test]
fn superblock_strings() {
    let s = Sample::load("superblock");
    let sample = named("payments", "superblock");
    let (payments, votes, indexes) = superblock_trigger(&s, sample);
    let with = |trigger: Trigger| PaymentsContext {
        triggers: vec![trigger],
        ..s.synced()
    };
    let funded = with(Trigger::superblock(s.height, &payments, votes.clone()));
    let r = s.run(&s.block, &funded);
    accept(&r);
    assert!(r.triggered);
    let coinbase = outs(&s.block);
    assert_eq!(r.superblock, indexes.iter().map(|&i| coinbase[i].clone()).collect::<Vec<_>>());
    let reward = r.block_subsidy + s.fee;

    // No trigger, or one short of votes: the block reward only.
    reject_value(&s.run(&s.block, &s.synced()), "no triggered superblock detected");
    let short = with(Trigger::superblock(s.height, &payments, votes[..votes.len() - 1].to_vec()));
    let r = s.run(&s.block, &short);
    assert!(!r.triggered);
    reject_value(&r, "no triggered superblock detected");
    // Abstain and no votes do not fund (the voters reach the threshold exactly).
    for outcome in [VOTE_OUTCOME_NO, VOTE_OUTCOME_ABSTAIN] {
        let mut changed = votes.clone();
        changed[0].outcome = outcome;
        reject_value(
            &s.run(&s.block, &with(Trigger::superblock(s.height, &payments, changed))),
            "no triggered superblock detected",
        );
    }
    // A trigger for another height is not this block's.
    let other = with(Trigger::superblock(s.height + 16616, &payments, votes.clone()));
    reject_value(&s.run(&s.block, &other), "no triggered superblock detected");

    // Triggered but not what the block pays.
    let mut wrong = payments.clone();
    wrong[0].1 += 1;
    // (IsBlockValueValid() writes this string only for a triggered superblock.)
    let r = s.run(&s.block, &with(Trigger::superblock(s.height, &wrong, votes.clone())));
    reject_value(&r, &format!("invalid superblock detected at height {}", s.height));
    // The payments out of order (IsValid() scans forward).
    let mut swapped = payments.clone();
    swapped.swap(0, 1);
    reject_value(
        &s.run(&s.block, &with(Trigger::superblock(s.height, &swapped, votes.clone()))),
        "invalid superblock detected",
    );

    // The bound, synced or not: blockReward + GetPaymentsLimit().
    let limit = oracle().superblock_payments_limit(&s.chain, s.height).unwrap();
    accept(&s.run(&s.paying(reward + limit), &s.ibd()));
    for pay in [s.ibd(), funded.clone()] {
        reject_value(&s.run(&s.paying(reward + limit + 1), &pay), "exceeded superblock max value");
    }

    // The leniencies: not synced, governance not loaded, or chainlocked, the triggered check
    // does not run.
    let unfunded = with(Trigger::superblock(s.height, &payments, vec![]));
    for pay in [
        PaymentsContext {
            synced: false,
            ..unfunded.clone()
        },
        PaymentsContext {
            governance_loaded: false,
            ..unfunded.clone()
        },
        PaymentsContext {
            best_chainlock_height: Some(s.height),
            ..unfunded.clone()
        },
    ] {
        accept(&s.run(&s.block, &pay));
    }
    // A chainlock below the block does not exempt it.
    let below = PaymentsContext {
        best_chainlock_height: Some(s.height - 1),
        ..unfunded
    };
    reject(&s.run(&s.block, &below), "bad-cb-amount");

    // A superblock that pays its payments but drops the masternode payment.
    let mut no_payee = s.block.clone();
    let mn = matched(&coinbase, &s.run(&s.block, &s.ibd()).masternode).unwrap();
    no_payee.txdata[0].output.remove(mn[0]);
    reject(&s.run(&no_payee, &funded), "bad-cb-payee");
}

/// v24's DisallowDuplicates: two equal payments must match two outputs (superblock.cpp:300-318,
/// the is_v24 scan start). Testnet, where V24 can activate; the payments are crafted, paid to
/// the block's miner address.
#[test]
fn superblock_duplicate_payments() {
    let s = Sample::load("testnet-superblock");
    let sample = named("payments", "testnet-superblock");
    let (_, votes, _) = superblock_trigger(&s, sample);
    let address = sample["addresses"][0].as_str().unwrap().to_string();
    let limit = oracle().superblock_payments_limit(&s.chain, s.height).unwrap();
    let amount = limit / 4;
    let pay = |payments: &[(String, i64)]| PaymentsContext {
        triggers: vec![Trigger::superblock(s.height, payments, votes.clone())],
        ..s.synced()
    };
    let paying = |copies: usize| {
        let mut block = s.block.clone();
        let miner = block.txdata[0].output[0].clone();
        for _ in 0..copies {
            let mut out = miner.clone();
            out.value = amount as u64;
            block.txdata[0].output.push(out);
        }
        block
    };
    let twice = pay(&[(address.clone(), amount), (address.clone(), amount)]);
    let r = s.run_v24(&paying(1), &twice, ThresholdState::Defined);
    assert_eq!(r.check, SuperblockCheck::AllowDuplicates);
    assert!(r.triggered);
    accept(&r);
    let r = s.run_v24(&paying(1), &twice, ThresholdState::Active);
    assert_eq!(r.check, SuperblockCheck::DisallowDuplicates);
    assert!(r.strict);
    reject_value(&r, &format!("invalid superblock detected at height {}", s.height));
    accept(&s.run_v24(&paying(2), &twice, ThresholdState::Active));
    accept(&s.run_v24(&paying(1), &pay(&[(address, amount)]), ThresholdState::Active));
}

/// v24's strict multiplicity (FindUnmatchedMasternodePayment()): the payee pays its 50%
/// operator share to its own payout script, so Core expects two equal outputs. Testnet (V24
/// can activate there); the fee is raised until the masternode reward splits evenly.
#[test]
fn strict_multiplicity() {
    let s = Sample::load("strict");
    let (fee, expected) = (s.fee..s.fee + 64)
        .find_map(|fee| {
            let pay = PaymentsContext {
                fee_reward: fee,
                ..s.ibd()
            };
            let mn = s.run_v24(&s.block, &pay, ThresholdState::Active).masternode;
            let n = mn.len();
            (n >= 2 && mn[n - 1] == mn[n - 2]).then_some((fee, mn))
        })
        .expect("an even masternode reward");
    let ibd = PaymentsContext {
        fee_reward: fee,
        ..s.ibd()
    };
    let synced = PaymentsContext {
        synced: true,
        ..ibd.clone()
    };
    let reward = s.run(&s.block, &ibd).block_subsidy + fee;
    // The coinbase: the miner, then the expected outputs with one or both equal ones.
    let coinbase = |outputs: &[TxOut]| {
        let mut block = s.block.clone();
        let miner = block.txdata[0].output[0].clone();
        let mut vout = vec![miner.clone()];
        for o in outputs {
            let mut out = miner.clone();
            out.value = o.value as u64;
            out.script_pubkey = o.script_pubkey.clone().into();
            vout.push(out);
        }
        vout[0].value = (reward - outputs.iter().map(|o| o.value).sum::<i64>()) as u64;
        block.txdata[0].output = vout;
        block
    };
    let one = coinbase(&expected[..expected.len() - 1]);
    let two = coinbase(&expected);
    for pay in [ibd, synced] {
        let r = s.run_v24(&one, &pay, ThresholdState::Defined);
        assert!(!r.strict);
        accept(&r);
        let r = s.run_v24(&one, &pay, ThresholdState::Active);
        assert!(r.strict);
        reject(&r, "bad-cb-payee");
        accept(&s.run_v24(&two, &pay, ThresholdState::Active));
    }
}

/// R4 F6: with no payee (every masternode PoSe-banned) GetBlockTxOuts() fails and
/// IsTransactionValid() accepts any payee.
#[test]
fn no_payee_is_accepted() {
    let s = Sample::load("banned");
    let r = s.run(&s.block, &s.synced());
    assert!(!r.txouts_ok && r.masternode.is_empty());
    accept(&r);
    let mut nothing = s.block.clone();
    nothing.txdata[0].output.truncate(1);
    accept(&s.run(&nothing, &s.synced()));
}

/// The MN_RR era (testnet): the platform share is an OP_RETURN output Core expects first.
#[test]
fn platform_share() {
    let s = Sample::load("platform");
    let r = s.run(&s.block, &s.ibd());
    accept(&r);
    assert_eq!(r.era, RewardEra::EvoReward);
    assert_eq!(r.masternode[0].script_pubkey, vec![0x6a], "OP_RETURN");
    assert!(matched(&outs(&s.block), &r.masternode).is_some());
    // PlatformShare(): 37.5% of the masternode part of the subsidy alone.
    let mn_subsidy =
        oracle().mn_payment(&s.chain, s.height, r.block_subsidy, RewardEra::EvoReward).unwrap();
    assert_eq!(r.masternode[0].value, mn_subsidy * 375 / 1000);
    let mut no_share = s.block.clone();
    let at = outs(&s.block).iter().position(|o| o.script_pubkey == [0x6a]).unwrap();
    no_share.txdata[0].output.remove(at);
    reject(&s.run(&no_share, &s.ibd()), "bad-cb-payee");
}

#[test]
fn payments_context_refusals() {
    let s = Sample::load("superblock");
    let sample = named("payments", "superblock");
    let (payments, votes, _) = superblock_trigger(&s, sample);
    let trigger = Trigger::superblock(s.height, &payments, votes.clone());
    let refused = |pay: PaymentsContext, what: &str| {
        let e = s.try_run(&s.block, &pay, ThresholdState::Defined).expect_err(what);
        assert_eq!(e.kind, ErrorKind::Argument, "{what}: {e:?}");
    };
    refused(
        PaymentsContext {
            fee_reward: -1,
            ..s.ibd()
        },
        "negative fees",
    );
    refused(
        PaymentsContext {
            triggers: vec![trigger.clone(), trigger.clone()],
            ..s.synced()
        },
        "the same trigger twice",
    );
    for (signal, outcome) in [
        (0, VOTE_OUTCOME_YES),
        (5, VOTE_OUTCOME_YES),
        (VOTE_SIGNAL_FUNDING, 0),
        (VOTE_SIGNAL_FUNDING, 4),
    ] {
        let mut bad = trigger.clone();
        bad.votes[0].signal = signal;
        bad.votes[0].outcome = outcome;
        refused(
            PaymentsContext {
                triggers: vec![bad],
                ..s.synced()
            },
            "a vote out of range",
        );
    }
    let mut twice = trigger.clone();
    twice.votes.push(twice.votes[0]);
    refused(
        PaymentsContext {
            triggers: vec![twice],
            ..s.synced()
        },
        "two votes of one masternode on one signal",
    );
    let mut garbage = trigger;
    garbage.object.push(0);
    refused(
        PaymentsContext {
            triggers: vec![garbage],
            ..s.synced()
        },
        "trailing bytes in a governance object",
    );
    refused(
        PaymentsContext {
            fee_reward: MAX_MONEY + 1,
            ..s.ibd()
        },
        "fees above MAX_MONEY",
    );
    // CSuperblock::IsExpired() adds the cycle to the event height as an int.
    refused(
        PaymentsContext {
            triggers: vec![Trigger::superblock(i32::MAX - 100, &payments, votes)],
            ..s.synced()
        },
        "an event height IsExpired() overflows",
    );
    // CheckBlock() rejects a block whose first transaction is not a coinbase first.
    let mut no_coinbase = s.block.clone();
    no_coinbase.txdata[0].input[0].previous_output.vout = 0; // no longer the null prevout
    assert!(!no_coinbase.txdata[0].is_coin_base());
    let e = s.try_run(&no_coinbase, &s.ibd(), ThresholdState::Defined).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "{e:?}");
}

/// Fees up to MAX_MONEY get a verdict: only the payee's multiplications are guarded, and the
/// strict sample's 50% operator reward does not overflow there.
#[test]
fn fees_up_to_max_money_get_a_verdict() {
    let s = Sample::load("strict");
    let pay = PaymentsContext {
        fee_reward: MAX_MONEY,
        ..s.ibd()
    };
    let r = s.run(&s.block, &pay);
    assert!(r.txouts_ok);
    assert!(r.masternode.iter().all(|o| o.value > 0));
}

/// A credit-pool step of the fixture.
struct Step {
    chain: Chain,
    height: i32,
    block: Vec<u8>,
    headers: Vec<[u8; 80]>,
    window_block: Vec<u8>,
    prev: CreditPool,
    expected: Value,
    deployments: Vec<(u8, ThresholdState)>,
}

impl Step {
    fn load(name: &str) -> Step {
        let s = named("credit", name);
        let pool = |p: &Value| CreditPool {
            locked: p["locked"].as_i64().unwrap(),
            current_limit: p["current_limit"].as_i64().unwrap(),
            lately_unlocked: p["lately_unlocked"].as_i64().unwrap(),
            indexes: UnlockIndexes::default(),
        };
        Step {
            chain: Chain::new(s["chain"].as_str().unwrap(), &[]),
            height: s["height"].as_i64().unwrap() as i32,
            block: hex(s["block"].as_str().unwrap()),
            headers: common::headers(s["headers"].as_str().unwrap()),
            window_block: hex(s["window_block"].as_str().unwrap()),
            prev: if s["prev"].is_null() {
                CreditPool::default()
            } else {
                pool(&s["prev"])
            },
            expected: s["expected"].clone(),
            deployments: s["deployments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| {
                    (
                        d[0].as_u64().unwrap() as u8,
                        ThresholdState::try_from(d[1].as_u64().unwrap() as u8).unwrap(),
                    )
                })
                .collect(),
        }
    }

    fn context(&self) -> Context {
        Context::new(
            self.height - self.headers.len() as i32,
            self.headers.clone(),
            false,
            self.deployments.clone(),
            0,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            MnList::Absent,
            vec![],
        )
    }

    fn run_with(&self, block: &[u8], step: &StepContext) -> Result<CreditPoolStep, Error> {
        oracle().credit_pool_step(&self.chain, block, &self.context(), step)
    }

    fn step(&self) -> StepContext {
        StepContext {
            prev: self.prev.clone(),
            blocks: vec![self.window_block.clone()],
        }
    }

    fn run(&self, step: &StepContext) -> CreditPoolStep {
        self.run_with(&self.block, step).unwrap()
    }

    fn cbtx_balance(&self) -> i64 {
        let block: Block = deserialize(&self.block).unwrap();
        block.txdata[0]
            .special_transaction_payload
            .clone()
            .unwrap()
            .to_coinbase_payload()
            .unwrap()
            .asset_locked_amount
            .unwrap() as i64
    }
}

fn step_reject(r: &CreditPoolStep, reason: &str) {
    assert_rejected(&r.verdict, reason, BLOCK_CONSENSUS, r);
}

fn pool_matches(pool: &CreditPool, expected: &Value) {
    assert_eq!(pool.locked, expected["locked"].as_i64().unwrap());
    assert_eq!(pool.current_limit, expected["current_limit"].as_i64().unwrap());
    assert_eq!(pool.lately_unlocked, expected["lately_unlocked"].as_i64().unwrap());
}

/// V20's activation block (mainnet 1,987,776): the first checked step, from the empty pool.
#[test]
fn credit_pool_at_v20_activation() {
    let s = Step::load("v20-activation");
    let r = s.run(&s.step());
    assert!(r.verdict.is_accept(), "{r:?}");
    assert!(r.checked);
    assert_eq!(r.expected, Some(s.cbtx_balance()));
    pool_matches(r.pool.as_ref().unwrap(), &s.expected);
}

/// A testnet block with an asset unlock (and the platform share): getcreditpoolinfo's pool, and
/// each credit-pool reject from the step context alone.
#[test]
fn credit_pool_unlock_block() {
    let s = Step::load("unlock");
    let block: Block = deserialize(&s.block).unwrap();
    let unlocks: Vec<u64> = block
        .txdata
        .iter()
        .filter(|tx| tx.tx_type() == TransactionType::AssetUnlock)
        .map(|tx| {
            tx.special_transaction_payload
                .clone()
                .unwrap()
                .to_asset_unlock_payload()
                .unwrap()
                .base
                .index
        })
        .collect();
    assert!(!unlocks.is_empty());
    let r = s.run(&s.step());
    assert!(r.verdict.is_accept(), "{r:?}");
    assert_eq!(r.expected, Some(s.cbtx_balance()));
    let pool = r.pool.unwrap();
    pool_matches(&pool, &s.expected);
    assert_eq!(pool.indexes, UnlockIndexes::from_indexes(unlocks.iter().copied()));

    // A different locked amount before the block: the CbTx disagrees.
    let mut step = s.step();
    step.prev.locked += 1;
    let r = s.run(&step);
    step_reject(&r, "bad-cbtx-assetlocked-amount");
    assert!(r.pool.is_none());
    // The unlocks above the limit.
    let mut step = s.step();
    step.prev.current_limit = 0;
    step_reject(&s.run(&step), "failed-creditpool-unlock-too-much");
    // The diff reads only prev, from the cache: a rejecting step needs no window-start block
    // (GetCreditPool(block), which reads it, runs only after the check accepts).
    step.blocks.clear();
    step_reject(&s.run(&step), "failed-creditpool-unlock-too-much");
    // An index the pool already spent.
    let mut step = s.step();
    step.prev.indexes = UnlockIndexes::from_indexes([unlocks[0]]);
    step_reject(&s.run(&step), "failed-creditpool-unlock-duplicated-index");
}

/// CE4-05 (ABI 29): rc.4 sums the diff with checked arithmetic (`creditpool.cpp` rc.4:297-349),
/// so a parent's pool outside the money range gets a verdict. A total that leaves `int64` makes
/// `GetTotalLocked()` throw, which `CheckCreditPoolDiffForBlock()` rejects as
/// `failed-checkcreditpooldiff` (`specialtxman.cpp` rc.4:1121-1124), with no expected balance.
/// Only an accepted step's pool construction, still unchecked (`creditpool.cpp` rc.4:98-108,
/// 215-227), refuses a `latelyUnlocked` outside the money range.
#[test]
fn credit_pool_checked_sums() {
    let s = Step::load("unlock");
    let mut step = s.step();
    step.prev.locked = MAX_MONEY + 1;
    let r = s.run(&step);
    step_reject(&r, "bad-cbtx-assetlocked-amount");
    assert_eq!(r.expected, Some(s.cbtx_balance() - s.prev.locked + MAX_MONEY + 1));
    // `locked - unlocked` below `int64`'s bottom.
    step.prev.locked = i64::MIN;
    let r = s.run(&step);
    step_reject(&r, "failed-checkcreditpooldiff");
    assert!(r.checked);
    assert_eq!((r.expected, r.pool), (None, None));
    // The widest limit: the step accepts.
    let mut step = s.step();
    step.prev.current_limit = i64::MAX;
    let r = s.run(&step);
    assert!(r.verdict.is_accept(), "{r:?}");
    assert_eq!(r.expected, Some(s.cbtx_balance()));
    // `latelyUnlocked` past the money range: refused where the step accepts, else a verdict.
    let mut step = s.step();
    step.prev.lately_unlocked = MAX_MONEY + 1;
    let e = s.run_with(&s.block, &step).unwrap_err();
    assert!(e.kind == ErrorKind::Argument && e.message.contains("latelyUnlocked"), "{e:?}");
    step.prev.current_limit = 0;
    step_reject(&s.run(&step), "failed-creditpool-unlock-too-much");
}

#[test]
fn credit_pool_refusals() {
    let s = Step::load("unlock");
    // ConstructCreditPool() reads the window start, which the context must hold whether or not
    // Core's per-block cache has it from an earlier call (run the step first to fill it).
    assert!(s.run(&s.step()).verdict.is_accept());
    let no_window = StepContext {
        prev: s.prev.clone(),
        blocks: vec![],
    };
    let e = s.run_with(&s.block, &no_window).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Context, "{e:?}");
    // Headers that start above the window start: the window start is a placeholder, whose block
    // the context cannot hold.
    let short = Context {
        first_height: s.height - s.headers.len() as i32 + 1,
        headers: s.headers[1..].to_vec(),
        ..s.context()
    };
    let e = oracle().credit_pool_step(&s.chain, &s.block, &short, &no_window).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Context, "{e:?}");
    // Disk blocks no node has: one without transactions, one whose first transaction is not a
    // coinbase, one that is no context header's, one given twice.
    let window: Block = deserialize(&s.window_block).unwrap();
    let mut empty = window.clone();
    empty.txdata.clear();
    let mut no_coinbase = window.clone();
    no_coinbase.txdata.remove(0);
    for (blocks, message) in [
        (vec![s.window_block.clone(), serialize(&empty)], "it has no coinbase"),
        (vec![serialize(&no_coinbase)], "it has no coinbase"),
        (vec![s.window_block.clone(), s.block.clone()], "not a context header's block"),
        (vec![s.window_block.clone(), s.window_block.clone()], "repeats a block"),
    ] {
        let step = StepContext {
            prev: s.prev.clone(),
            blocks,
        };
        let e = s.run_with(&s.block, &step).unwrap_err();
        assert!(e.kind == ErrorKind::Argument && e.message.contains(message), "{e:?}");
    }
    // An asset unlock whose payload does not decode, in a disk block its header commits to: the
    // window block with the step's unlock appended, its payload cut to the version byte, and the
    // context's headers relinked above it (context headers are not proof-of-work checked).
    let unlock_block: Block = deserialize(&s.block).unwrap();
    let unlock =
        unlock_block.txdata.iter().find(|tx| tx.tx_type() == TransactionType::AssetUnlock).unwrap();
    let mut bad = serialize(unlock);
    let payload_len = common::extra_payload(&bad).len();
    assert!(payload_len < 0xfd);
    bad.truncate(bad.len() - payload_len - 1);
    bad.extend([1, 1]);
    let mut txs: Vec<Vec<u8>> = window.txdata.iter().map(serialize).collect();
    txs.push(bad);
    let leaves: Vec<[u8; 32]> =
        txs.iter().map(|tx| sha256d::Hash::hash(tx).to_byte_array()).collect();
    let (root, _) = oracle().merkle_root(&leaves).unwrap();
    let block_hash = |header: &[u8; 80]| {
        deserialize::<dashcore::block::Header>(header).unwrap().block_hash().to_byte_array()
    };
    let mut headers = s.headers.clone();
    headers[0][36..68].copy_from_slice(&root);
    for i in 1..headers.len() {
        let prev = block_hash(&headers[i - 1]);
        headers[i][4..36].copy_from_slice(&prev);
    }
    let mut block = s.block.clone();
    block[4..36].copy_from_slice(&block_hash(headers.last().unwrap()));
    assert!(txs.len() < 0xfd);
    let mut crafted = headers[0].to_vec();
    crafted.push(txs.len() as u8);
    txs.iter().for_each(|tx| crafted.extend(tx));
    let relinked = Context {
        headers,
        ..s.context()
    };
    let step = StepContext {
        prev: s.prev.clone(),
        blocks: vec![crafted],
    };
    let e = oracle().credit_pool_step(&s.chain, &block, &relinked, &step).unwrap_err();
    assert!(
        e.kind == ErrorKind::Argument && e.message.contains("payload does not decode"),
        "{e:?}"
    );
    // A block whose merkle root does not commit to its transactions.
    let mut block: Block = deserialize(&s.block).unwrap();
    let n = block.txdata.len();
    assert!(n > 2, "the unlock block has transactions to reorder");
    block.txdata.swap(1, n - 1);
    let e = s.run_with(&serialize(&block), &s.step()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "{e:?}");
    // A pool before V20 that is not empty.
    let v20 = Step::load("v20-activation");
    let e = v20
        .run_with(
            &v20.block,
            &StepContext {
                prev: CreditPool {
                    locked: 1,
                    ..CreditPool::default()
                },
                blocks: vec![v20.window_block.clone()],
            },
        )
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument, "{e:?}");
}
