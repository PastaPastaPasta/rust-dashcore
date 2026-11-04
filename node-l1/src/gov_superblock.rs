//! GV1a's `Superblock::is_valid` against Core's `CSuperblock::IsValid` (GV1a r1 Sol finding 3), on
//! the mined superblocks of O2a-GV's payments fixture ([`crate::payments`]).
//!
//! Each case funds a trigger and builds a coinbase around its payments; Core runs
//! `oracle_block_value_payee` (synced, governance loaded, no ChainLock), whose value and payee
//! checks call `CSuperblock::IsValid` on the triggered superblock. GV1a parses the same trigger
//! object with `Superblock::parse` and answers with `Superblock::is_valid`. Compared: Core's
//! value verdict (and its "invalid superblock" error), its payee verdict, whether the superblock
//! is triggered and the payments it selects (`GetSuperblockPayments` against `tx_outs`). Each
//! case runs with V24 defined and active after the tip, so both `is_v24` matching modes are
//! reached.
//!
//! Core reaches `IsValid` only for a trigger at the block's height (`IsSuperblockTriggered`), so
//! its height branch is compared through the trigger's outcome: a trigger one block off is never
//! triggered, and GV1a's superblock is invalid at its own (off) height.

use dash_consensus::{Amount, Tx};
use dash_evo::payments::invalid_superblock_error;
use dash_governance::GovParams;
use dash_governance::trigger::{Superblock, is_valid_block_height, payments_limit};
use dash_oracle_sys::{Oracle, PaymentsContext, Pin, SuperblockCheck, ThresholdState};
use dash_primitives::{GovObject, TxOut};
use dash_types::codec::BaseCodec;

use crate::payments::{Case, Payments, samples, superblock_case};
use crate::{Divergence, check, divergence};

/// One coinbase around one trigger.
pub struct SuperblockCase {
    pub name: String,
    /// The edit: "exact", "missing", "reordered", ….
    pub kind: String,
    pub case: Case,
    /// The trigger's `event_block_height`: the block's own, or one off.
    pub event_height: i32,
}

/// What Core and GV1a agreed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuperblockAnswer {
    /// `CSuperblock::IsValid`'s verdict; `None` where Core never calls it (no trigger at the
    /// height, or the superblock maximum failed first).
    pub valid: Option<bool>,
    /// The matching mode: V24's distinct outputs.
    pub is_v24: bool,
}

/// The cases on every mined superblock of the fixture, each with V24 defined and active.
pub fn superblock_cases() -> Vec<SuperblockCase> {
    let mut cases = Vec::new();
    for index in 0..samples().len() {
        let Some((mut case, payments)) = superblock_case(index) else {
            continue;
        };
        case.trigger = Some(payments.clone());
        let variants = variants(&case, &payments);
        for v24 in [ThresholdState::Defined, ThresholdState::Active] {
            for (name, coinbase, payments, event_height) in variants.iter().cloned() {
                let Ok(mut case) = Case::mined(index) else {
                    continue;
                };
                case.coinbase = coinbase;
                case.v24 = v24;
                case.pay.synced = true;
                case.trigger = Some(payments);
                cases.push(SuperblockCase {
                    name: format!("{} {name} v24 {v24:?}", case.sample().name),
                    kind: name,
                    case,
                    event_height,
                });
            }
        }
    }
    cases
}

/// The coinbase's index of the output a payment's `TxOut` is.
fn output_of(coinbase: &dash_primitives::Transaction, out: &TxOut) -> Option<usize> {
    coinbase.outputs.iter().position(|o| o == out)
}

/// The edits: (name, coinbase, trigger payments, event height).
fn variants(
    case: &Case,
    payments: &Payments,
) -> Vec<(String, dash_primitives::Transaction, Payments, i32)> {
    let s = case.sample();
    let h = s.height;
    let mut out = vec![];
    let Some(p) = gov_params(case) else {
        return out;
    };
    let Some(outs) = tx_outs(case, &p, h) else {
        return out;
    };
    let Ok(limit) = payments_limit(h, &p) else {
        return out;
    };
    let base = case.coinbase.clone();
    let total: Amount = payments.iter().map(|(_, a)| a).sum();
    let mut push =
        |name: &str, coinbase: dash_primitives::Transaction, payments: Payments, at: i32| {
            out.push((name.to_owned(), coinbase, payments, at));
        };
    push("exact", base.clone(), payments.clone(), h);
    push("off-height", base.clone(), payments.clone(), h + 1);

    let Some(j0) = output_of(&base, &outs[0]) else {
        return out;
    };
    let mut missing = base.clone();
    missing.outputs.remove(j0);
    push("missing", missing, payments.clone(), h);

    // Two distinct payments to reorder: a second one to the first payment's script when the
    // trigger has one payment (it pays 1 DASH more, so it is distinct).
    let (mut two, mut two_payments) = (base.clone(), payments.clone());
    let second = match outs.get(1) {
        Some(second) => second.clone(),
        None => {
            let extra = TxOut {
                value: outs[0].value + dash_consensus::subsidy::COIN,
                ..outs[0].clone()
            };
            two.outputs.push(extra.clone());
            two_payments.push((payments[0].0.clone(), extra.value));
            extra
        }
    };
    push("two in order", two.clone(), two_payments.clone(), h);
    if second != outs[0]
        && let Some(j1) = output_of(&two, &second)
    {
        let mut reordered = two.clone();
        reordered.outputs.swap(j0, j1);
        push("reordered", reordered, two_payments, h);
    }

    let smallest = (0..payments.len()).min_by_key(|&k| payments[k].1);
    // The smallest payment listed twice: one output serves both before V24 only.
    if let Some(k) = smallest
        && total + payments[k].1 <= limit
        && let Some(jk) = output_of(&base, &outs[k])
    {
        let mut dup = payments.clone();
        dup.insert(k + 1, payments[k].clone());
        push("duplicate, one output", base.clone(), dup.clone(), h);
        let mut both = base.clone();
        both.outputs.insert(jk + 1, base.outputs[jk].clone());
        push("duplicate, two outputs", both, dup, h);
    }

    // One payment repeated once per coinbase output, then once more, on a coinbase paying it
    // alone: before V24 a single output serves every repeat, so only the output count rejects.
    if let Some(k) = smallest {
        let mut alone = base.clone();
        alone.outputs.retain(|o| *o == outs[k] || !outs.contains(o));
        let n = alone.outputs.len();
        for (name, repeats) in [("repeated per output", n), ("repeated past the outputs", n + 1)] {
            if payments[k].1 * repeats as Amount <= limit {
                push(name, alone.clone(), vec![payments[k].clone(); repeats], h);
            }
        }
    }

    // The first payment raised so the total is the limit, then one past it (the miner takes one
    // duff less, so the coinbase stays within the superblock maximum).
    for (name, raise, miner_cut) in
        [("budget at the limit", limit - total, 0), ("budget over the limit", limit - total + 1, 1)]
    {
        let mut coinbase = base.clone();
        let mut raised = payments.clone();
        raised[0].1 += raise;
        coinbase.outputs[j0].value += raise;
        coinbase.outputs[0].value -= miner_cut;
        push(name, coinbase, raised, h);
    }

    // The miner takes one duff more than the block reward.
    let mut over = base.clone();
    over.outputs[0].value += 1;
    push("over the block value", over, payments.clone(), h);
    out
}

fn gov_params(case: &Case) -> Option<GovParams> {
    GovParams::from_chain_params(case.sample().net.params).ok()
}

/// The case's trigger payments as GV1a parses them from the object Core is given.
fn tx_outs(case: &Case, p: &GovParams, event_height: i32) -> Option<Vec<TxOut>> {
    Some(parse_trigger(&case.payments_context_at(event_height), p).ok()?.tx_outs())
}

fn parse_trigger(pay: &PaymentsContext, p: &GovParams) -> Result<Superblock, Divergence> {
    let object = &pay.triggers.first().ok_or_else(|| divergence("no trigger"))?.object;
    let obj = GovObject::decode(&mut &object[..]).map_err(|_| divergence("trigger decode"))?;
    Superblock::parse(&obj, p).map_err(|_| divergence("trigger parse"))
}

/// Core's verdict on `c` against GV1a's.
pub fn compare_superblock(c: &SuperblockCase) -> Result<SuperblockAnswer, Divergence> {
    let case = &c.case;
    let s = case.sample();
    let p = gov_params(case).ok_or_else(|| divergence("governance parameters"))?;
    let h = s.height;
    let height = u32::try_from(h).map_err(|_| divergence("height"))?;
    let pay = case.payments_context_at(c.event_height);
    let sb = parse_trigger(&pay, &p)?;
    let core = Oracle::new(Pin::V24_0)
        .block_value_payee(
            &(s.net.chain)(),
            &s.block_with(&case.coinbase),
            &s.context(case.v24, case.list_bytes.clone()),
            &pay,
        )
        .map_err(|_| divergence("oracle error"))?;
    check(core.check != SuperblockCheck::NoCheck, "a full check")?;
    let is_v24 = core.check == SuperblockCheck::DisallowDuplicates;
    // `ConnectBlock`'s reward: the subsidy (EV3's `diff_block_value_payee` compares it) and fees.
    let block_reward = core.block_subsidy + s.fee;
    let coinbase = Tx::new(case.coinbase.clone());
    let ours = sb
        .is_valid(&coinbase, u32::try_from(sb.height).unwrap_or(u32::MAX), block_reward, is_v24, &p)
        .map_err(|_| divergence("is_valid fault"))?;

    if sb.height != h {
        // Off the height: Core never triggers it, so the coinbase paying it is over the block
        // reward; GV1a's superblock is not valid at its own height either.
        check(!is_valid_block_height(sb.height, &p), "off-height trigger height")?;
        check(!ours, "off-height superblock valid")?;
        check(!core.value_valid, "off-height value check")?;
        check(
            core.value_error.ends_with(", no triggered superblock detected"),
            "off-height error",
        )?;
        return Ok(SuperblockAnswer {
            valid: None,
            is_v24,
        });
    }
    if !core.value_valid && core.value_error != invalid_superblock_error(height) {
        // The superblock maximum failed before `IsValid`.
        check(core.value_error.ends_with(", exceeded superblock max value"), "value error")?;
        return Ok(SuperblockAnswer {
            valid: None,
            is_v24,
        });
    }
    check(core.value_valid == ours, "IsValid (value check)")?;
    if ours {
        check(core.triggered, "triggered")?;
        let selected: Vec<TxOut> = core
            .superblock
            .iter()
            .map(|o| TxOut {
                value: o.value,
                script_pubkey: o.script_pubkey.clone().into(),
            })
            .collect();
        check(selected == sb.tx_outs(), "selected payments")?;
        check(core.payee_valid == Some(true), "IsValid (payee check)")?;
    }
    Ok(SuperblockAnswer {
        valid: Some(ours),
        is_v24,
    })
}
