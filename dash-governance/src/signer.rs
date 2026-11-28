//! `GovernanceSigner`'s decisions (GS1 D1), pure: the superblock plan a masternode would put in a
//! trigger at a tip, the unsigned trigger, and which triggers it votes on. Keys, clocks, the
//! masternode lists and the manager's objects come in as arguments; signing, scheduling and the
//! votes themselves are the node's.
//!
//! Heights, times and amounts use checked or saturating arithmetic in the GV1a D13/D14/D40 style
//! (GS1 D6): a height that overflows gives no plan or no vote, an epoch window that overflows
//! skips its proposal.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use dash_primitives::{GovObject, OutPoint, TxHash};
use dash_types::Numeric;

use crate::GovParams;
use crate::object::{LoadedType, json_object, load_data};
use crate::strings::decode_destination;
use crate::tally::VoteInstance;
use crate::trigger::{Payment, Status, Superblock, parse_payment_amount, payments_limit};
use crate::vote::{OUTCOME_YES, SIGNAL_FUNDING};

/// `GOVERNANCE_FUDGE_WINDOW`: two hours either side of a proposal's epochs.
// Core: governance/signing.cpp:23
pub const FUDGE_WINDOW: i64 = 2 * 60 * 60;

/// The signer's own state: `votedFundingYesTriggerHash`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SignerState {
    /// The trigger this masternode voted YES-FUNDING for in the current cycle, internal order.
    pub voted_yes: Option<[u8; 32]>,
}

/// The gates the superblock plan reads besides the height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanGates {
    /// `m_govman.IsValid()`.
    pub governance_valid: bool,
    /// `m_mn_sync.IsSynced()`.
    pub synced: bool,
    /// `HasAlreadyVotedFundingTrigger()`.
    pub voted_yes: bool,
}

/// One entry of `GetApprovedProposals(GetListAtChainTip())`, in that order.
#[derive(Clone, Copy, Debug)]
pub struct ApprovedProposal<'a> {
    /// `GetHash()`, internal order.
    pub hash: [u8; 32],
    /// The object's data (`vchData`), read with `GetJSONObject()`.
    pub data: &'a [u8],
}

/// An exception Core's superblock plan does not catch (an unreadable approved proposal), with
/// its `what()`. Unreachable for objects the manager admitted; the node treats it as fatal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposalReadError {
    pub proposal: [u8; 32],
    pub what: String,
}

/// `UintToArith256(a) <=> UintToArith256(b)`: the internal bytes compared from the last.
pub fn arith_cmp(a: &[u8; 32], b: &[u8; 32]) -> Ordering {
    a.iter().rev().cmp(b.iter().rev())
}

/// `nHeight % nSuperblockCycle >= nSuperblockCycle - nSuperblockMaturityWindow`; false where the
/// cycle is 0 or the subtraction overflows.
// Core: governance/signing.cpp:42-44
pub fn in_maturity_window(height: i32, p: &GovParams) -> bool {
    let cycle = p.superblock_cycle();
    match (height.checked_rem(cycle), cycle.checked_sub(p.superblock_maturity_window())) {
        (Some(offset), Some(start)) => offset >= start,
        _ => false,
    }
}

/// `GetNearestSuperblocksHeights` with checked arithmetic; `None` where a step overflows.
// Core: governance/superblock.cpp:126-143
fn nearest_superblock_heights_checked(height: i32, p: &GovParams) -> Option<(i32, i32)> {
    let (start, cycle) = (p.superblock_start(), p.superblock_cycle());
    let offset = cycle.checked_sub(start.checked_rem(cycle)?)?.checked_rem(cycle)?;
    let first = start.checked_add(offset)?;
    if height < first {
        Some((0, first))
    } else {
        let last = height.checked_sub(height.checked_rem(cycle)?)?;
        Some((last, last.checked_add(cycle)?))
    }
}

/// `SBEpochTime`: `now + (next - height) * 2.62 * 60` evaluated in `f64` in that order, then
/// truncated toward zero (the cast saturates at the `i64` bounds).
// Core: v24.0.3 governance/signing.cpp:59-60; governance/superblock.cpp:436-440
pub fn sb_epoch_time(now: i64, next: i32, height: i32) -> Option<i64> {
    let blocks = next.checked_sub(height)?;
    Some((now as f64 + f64::from(blocks) * 2.62 * 60.0) as i64)
}

/// Core's superblock plan at tip `height` (`signing.cpp:38-119`): the payments the approved
/// proposals get in the next superblock, filled greedily within its budget, or `None`.
///
/// `approved` is called only once the gates pass, as Core reads the list after them. `now` is
/// `GetTime()` (the mockable clock, not adjusted time).
// Core: governance/signing.cpp:38-119
pub fn superblock_plan<'a>(
    height: i32,
    gates: PlanGates,
    approved: impl FnOnce() -> Vec<ApprovedProposal<'a>>,
    now: i64,
    p: &GovParams,
) -> Result<Option<Superblock>, ProposalReadError> {
    if !gates.governance_valid || !gates.synced || !in_maturity_window(height, p) || gates.voted_yes
    {
        return Ok(None);
    }
    let approved = approved();
    if approved.is_empty() {
        return Ok(None);
    }
    let Some((_, next)) = nearest_superblock_heights_checked(height, p) else {
        return Ok(None);
    };
    let Some(sb_epoch_time) = sb_epoch_time(now, next, height) else {
        return Ok(None);
    };
    let Ok(budget) = payments_limit(next, p) else {
        return Ok(None);
    };

    let mut payments = Vec::new();
    let mut allocated: i64 = 0;
    for proposal in approved {
        let fail = |what: String| ProposalReadError {
            proposal: proposal.hash,
            what,
        };
        let json = json_object(proposal.data).map_err(fail)?;
        let address = json.get(b"payment_address").map_err(fail)?.get_val_str();
        let Some(dest) = decode_destination(address, p.chain()) else {
            continue;
        };
        let amount_text = json.get(b"payment_amount").map_err(fail)?.get_val_str();
        let Ok(amount) = parse_payment_amount(amount_text) else {
            continue;
        };
        // `CGovernancePayment::IsValid()` holds for every destination `DecodeDestination` accepts
        // (`superblock.cpp:415-432`).
        if allocated.saturating_add(amount) > budget {
            continue;
        }
        let start = json.get(b"start_epoch").and_then(|v| v.get_i64()).map_err(fail)?;
        let end = json.get(b"end_epoch").and_then(|v| v.get_i64()).map_err(fail)?;
        let (Some(window_start), Some(window_end)) =
            (start.checked_sub(FUDGE_WINDOW), end.checked_add(FUDGE_WINDOW))
        else {
            continue;
        };
        if sb_epoch_time < window_start || sb_epoch_time > window_end {
            continue;
        }
        allocated = allocated.saturating_add(amount);
        payments.push(Payment {
            dest,
            amount,
            proposal_hash: proposal.hash,
        });
    }
    if payments.is_empty() {
        return Ok(None);
    }
    payments.sort_by(|a, b| arith_cmp(&b.proposal_hash, &a.proposal_hash));
    Ok(Some(Superblock {
        height: next,
        payments,
        status: Status::Valid,
    }))
}

/// `CGovernanceObject(uint256(), 1, adjusted, uint256(), sb.GetHexStrData())`: the unsigned
/// trigger, with a null masternode outpoint and no signature.
// Core: governance/signing.cpp:130; governance/common.cpp:12-19; governance/object.cpp:341-348
pub fn trigger_template(sb: &Superblock, adjusted: i64, p: &GovParams) -> GovObject {
    let data = json_bytes(sb, p);
    let obj_type = match load_data(&data) {
        LoadedType::Type(t) => t,
        LoadedType::Unchanged | LoadedType::Unparsable => 0,
    };
    let zero = TxHash::from_lendian([0; 32]);
    GovObject::new(
        zero,
        1,
        adjusted,
        zero,
        data,
        Numeric::from_base(obj_type),
        OutPoint::new(zero, u32::MAX),
        Vec::new(),
    )
}

/// `ParseHex(GetHexStrData())`: the trigger's JSON text.
fn json_bytes(sb: &Superblock, p: &GovParams) -> Vec<u8> {
    let hex = sb.hex_str_data(p);
    crate::strings::parse_hex(hex.as_bytes())
}

/// What the YES branch does for the trigger this masternode wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YesDecision {
    /// A FUNDING vote of ours is already current: no vote; `yes` says whether it is YES, in which
    /// case the hash becomes the voted-yes trigger.
    AlreadyVoted {
        yes: bool,
    },
    /// Vote YES-FUNDING now.
    Vote,
}

/// Our current votes on the trigger (`GetCurrentMNVotes(outpoint)`, `None` without a record).
// Core: governance/signing.cpp:195-213
pub fn yes_decision(record: Option<&BTreeMap<i32, VoteInstance>>) -> YesDecision {
    match record.and_then(|instances| instances.get(&SIGNAL_FUNDING)) {
        Some(instance) => YesDecision::AlreadyVoted {
            yes: instance.outcome == OUTCOME_YES,
        },
        None => YesDecision::Vote,
    }
}

/// One entry of `GetActiveTriggers()`, in `m_triggers` order.
#[derive(Clone, Copy, Debug)]
pub struct ActiveTrigger<'a> {
    /// `GetGovernanceObjHash()`, internal order.
    pub hash: [u8; 32],
    /// `GetBlockHeight()`.
    pub height: i32,
    /// Whether `FindGovernanceObject(hash)` finds the object.
    pub known: bool,
    /// Our current votes on it (`GetCurrentMNVotes(outpoint)`), `None` without a record.
    pub our_record: Option<&'a BTreeMap<i32, VoteInstance>>,
}

/// The triggers to vote NO-FUNDING on, in order: the active ones that are known, not at or
/// below the cached height, inside their maturity window at `height`, not the voted-yes
/// trigger, and without a current FUNDING vote of ours. `voted_yes` is the hash as the YES
/// step of the same call left it.
// Core: governance/signing.cpp:229-278
pub fn no_vote_plan(
    triggers: &[ActiveTrigger<'_>],
    cached_height: i32,
    height: i32,
    voted_yes: Option<[u8; 32]>,
    p: &GovParams,
) -> Vec<[u8; 32]> {
    let window = p.superblock_maturity_window();
    triggers
        .iter()
        .filter(|t| t.known)
        .filter(|t| t.height > cached_height)
        .filter(|t| t.height.checked_sub(window).is_some_and(|start| height >= start))
        .filter(|t| voted_yes != Some(t.hash))
        .filter(|t| !t.our_record.is_some_and(|r| r.contains_key(&SIGNAL_FUNDING)))
        .map(|t| t.hash)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::data_hash;
    use crate::strings::Destination;
    use crate::test_support::params;
    use crate::trigger::nearest_superblock_heights;
    use crate::vote::OUTCOME_NO;
    use dash_params::Network;

    const NOW: i64 = 1_700_000_000;

    fn open() -> PlanGates {
        PlanGates {
            governance_valid: true,
            synced: true,
            voted_yes: false,
        }
    }

    /// The first height of the maturity window before the superblock after the start block.
    fn window_start(p: &GovParams) -> (i32, i32) {
        let (_, next) = nearest_superblock_heights(p.superblock_start(), p);
        let next = next.checked_add(p.superblock_cycle()).expect("height");
        (next.checked_sub(p.superblock_maturity_window()).expect("height"), next)
    }

    fn address(p: &GovParams, byte: u8) -> String {
        Destination::PubKeyHash([byte; 20]).encode(p.chain())
    }

    /// Proposal data with the amount and epochs as raw JSON tokens.
    fn proposal(addr: &str, amount: &str, start: i64, end: i64) -> Vec<u8> {
        format!(
            "{{\"end_epoch\":{end},\"name\":\"p\",\"payment_address\":\"{addr}\",\"payment_amount\":{amount},\"start_epoch\":{start},\"type\":1,\"url\":\"https://x\"}}"
        )
        .into_bytes()
    }

    fn hash(last: u8, first: u8) -> [u8; 32] {
        let mut h = [0u8; 32];
        h[0] = first;
        h[31] = last;
        h
    }

    fn plan(
        p: &GovParams,
        height: i32,
        gates: PlanGates,
        approved: &[([u8; 32], Vec<u8>)],
        now: i64,
    ) -> Result<Option<Superblock>, ProposalReadError> {
        let list = || {
            approved
                .iter()
                .map(|(hash, data)| ApprovedProposal {
                    hash: *hash,
                    data,
                })
                .collect()
        };
        superblock_plan(height, gates, list, now, p)
    }

    fn paid(sb: &Option<Superblock>) -> Vec<[u8; 32]> {
        sb.as_ref()
            .map(|sb| sb.payments.iter().map(|x| x.proposal_hash).collect())
            .unwrap_or_default()
    }

    /// An epoch window around the superblock time at `height`.
    fn around(p: &GovParams, height: i32) -> (i64, i64) {
        let (_, next) = nearest_superblock_heights(height, p);
        let t = sb_epoch_time(NOW, next, height).expect("time");
        (t - 1000, t + 1000)
    }

    #[test]
    fn maturity_window_accessor() {
        assert_eq!(params(Network::Main).superblock_maturity_window(), 1662);
        assert_eq!(params(Network::Testnet3).superblock_maturity_window(), 8);
        assert_eq!(params(Network::Regtest).superblock_maturity_window(), 10);
    }

    #[test]
    fn fudge_window_is_two_hours() {
        assert_eq!(FUDGE_WINDOW, 7200);
    }

    #[test]
    fn gates() {
        let p = params(Network::Regtest);
        let (h, _) = window_start(&p);
        let (s, e) = around(&p, h);
        let one = [(hash(1, 0), proposal(&address(&p, 1), "1", s, e))];
        assert!(plan(&p, h, open(), &one, NOW).unwrap().is_some());
        for gates in [
            PlanGates {
                governance_valid: false,
                ..open()
            },
            PlanGates {
                synced: false,
                ..open()
            },
            PlanGates {
                voted_yes: true,
                ..open()
            },
        ] {
            let called = std::cell::Cell::new(false);
            let r = superblock_plan(
                h,
                gates,
                || {
                    called.set(true);
                    Vec::new()
                },
                NOW,
                &p,
            );
            assert_eq!(r, Ok(None), "{gates:?}");
            assert!(!called.get(), "the approved list is read after the gates");
        }
        // The window: `h % cycle >= cycle - window`.
        assert!(in_maturity_window(h, &p));
        assert!(!in_maturity_window(h - 1, &p));
        let called = std::cell::Cell::new(false);
        let before = superblock_plan(
            h - 1,
            open(),
            || {
                called.set(true);
                Vec::new()
            },
            NOW,
            &p,
        );
        assert_eq!(before, Ok(None));
        assert!(!called.get(), "the approved list is read after the maturity gate");
        assert!(in_maturity_window(h + p.superblock_maturity_window() - 1, &p));
        assert_eq!(plan(&p, h - 1, open(), &one, NOW), Ok(None));
        assert!(
            plan(&p, h + p.superblock_maturity_window() - 1, open(), &one, NOW).unwrap().is_some()
        );
    }

    #[test]
    fn empty_and_invalid_destination() {
        let p = params(Network::Regtest);
        let (h, _) = window_start(&p);
        let (s, e) = around(&p, h);
        assert_eq!(plan(&p, h, open(), &[], NOW), Ok(None));
        let mainnet = address(&params(Network::Main), 1);
        let bad =
            [(hash(1, 0), proposal(&mainnet, "1", s, e)), (hash(2, 0), proposal("", "1", s, e))];
        assert_eq!(plan(&p, h, open(), &bad, NOW), Ok(None));
        let mut mixed = bad.to_vec();
        mixed.push((hash(3, 0), proposal(&address(&p, 3), "1", s, e)));
        assert_eq!(paid(&plan(&p, h, open(), &mixed, NOW).unwrap()), vec![hash(3, 0)]);
    }

    /// `ParsePaymentAmount` throws for an empty amount, an exponent and more than 20 characters;
    /// the proposal is skipped and the rest go on. `getValStr()` is the token's raw text.
    #[test]
    fn amount_parse_throws_skip() {
        let p = params(Network::Regtest);
        let (h, _) = window_start(&p);
        let (s, e) = around(&p, h);
        let addr = address(&p, 1);
        let ok = (hash(9, 0), proposal(&addr, "\"2.5\"", s, e));
        for amount in ["\"\"", "1e5", "100000000000000000000", "\"000000000000000000001\""] {
            let r = plan(
                &p,
                h,
                open(),
                &[(hash(1, 0), proposal(&addr, amount, s, e)), ok.clone()],
                NOW,
            )
            .unwrap()
            .expect("the other proposal is paid");
            assert_eq!(r.payments.len(), 1, "{amount}");
            assert_eq!(r.payments[0].amount, 250_000_000);
        }
        // The amount key missing reads "" (`NullUniValue`).
        let no_amount = format!(
            "{{\"end_epoch\":{e},\"payment_address\":\"{addr}\",\"start_epoch\":{s},\"type\":1}}"
        );
        assert_eq!(plan(&p, h, open(), &[(hash(1, 0), no_amount.into_bytes())], NOW), Ok(None));
        // A number token's text is read as is.
        let r = plan(&p, h, open(), &[(hash(1, 0), proposal(&addr, "1.25", s, e))], NOW).unwrap();
        assert_eq!(r.expect("paid").payments[0].amount, 125_000_000);
    }

    /// A proposal over the remaining budget is skipped and the fill goes on with the next.
    #[test]
    fn greedy_fill_skips_and_continues() {
        let p = params(Network::Regtest);
        let (h, next) = window_start(&p);
        let budget = payments_limit(next, &p).unwrap();
        assert!(budget > 3);
        let (s, e) = around(&p, h);
        let amt = |duffs: i64| crate::strings::value_from_amount(duffs);
        let addr = address(&p, 1);
        let list = [
            (hash(1, 0), proposal(&addr, &amt(budget - 2), s, e)),
            (hash(2, 0), proposal(&addr, &amt(3), s, e)),
            (hash(3, 0), proposal(&addr, &amt(2), s, e)),
            (hash(4, 0), proposal(&addr, &amt(1), s, e)),
        ];
        let sb = plan(&p, h, open(), &list, NOW).unwrap().expect("plan");
        let mut got: Vec<_> = sb.payments.iter().map(|x| (x.proposal_hash, x.amount)).collect();
        got.sort();
        assert_eq!(got, vec![(hash(1, 0), budget - 2), (hash(3, 0), 2)]);
        assert_eq!(sb.height, next);
        assert_eq!(sb.status, Status::Valid);
        // Exactly the budget fits.
        let exact = [(hash(1, 0), proposal(&addr, &amt(budget), s, e))];
        assert!(plan(&p, h, open(), &exact, NOW).unwrap().is_some());
        let over = [(hash(1, 0), proposal(&addr, &amt(budget + 1), s, e))];
        assert_eq!(plan(&p, h, open(), &over, NOW), Ok(None));
    }

    #[test]
    fn window_edges() {
        let p = params(Network::Regtest);
        let (h, next) = window_start(&p);
        let t = sb_epoch_time(NOW, next, h).unwrap();
        let addr = address(&p, 1);
        let run = |start: i64, end: i64| {
            plan(&p, h, open(), &[(hash(1, 0), proposal(&addr, "1", start, end))], NOW)
                .unwrap()
                .is_some()
        };
        // start_epoch - 7200 == SBEpochTime and end_epoch + 7200 == SBEpochTime are inside.
        assert!(run(t + FUDGE_WINDOW, t + FUDGE_WINDOW + 10));
        assert!(run(t - FUDGE_WINDOW - 10, t - FUDGE_WINDOW));
        // One second beyond either end is outside.
        assert!(!run(t + FUDGE_WINDOW + 1, t + FUDGE_WINDOW + 10));
        assert!(!run(t - FUDGE_WINDOW - 10, t - FUDGE_WINDOW - 1));
    }

    /// `(double)now + (next - h) * 2.62 * 60` truncated: values from Core's expression in that
    /// order. At `now = 0`, an odd `next - h` of 25 gives 3930 here and 3929 in
    /// `(next - h) * 157.2`; a large `now` rounds that difference away in the sum.
    #[test]
    fn sb_epoch_time_f64() {
        assert_eq!(sb_epoch_time(0, 125, 100), Some(3930));
        assert_eq!(sb_epoch_time(0, 145, 100), Some(7074));
        assert_eq!(sb_epoch_time(0, 101, 100), Some(157));
        assert_eq!(sb_epoch_time(NOW, 101, 100), Some(1_700_000_157));
        assert_eq!(sb_epoch_time(NOW, 125, 100), Some(1_700_003_930));
        assert_eq!(sb_epoch_time(NOW, 145, 100), Some(1_700_007_074));
        assert_eq!(sb_epoch_time(NOW, 1761, 100), Some(1_700_261_109));
        // Toward zero, not floor.
        assert_eq!(sb_epoch_time(-1000, 101, 100), Some(-842));
        // `now` is rounded to a double before the sum.
        assert_eq!(sb_epoch_time(9_007_199_254_740_995, 101, 100), Some(9_007_199_254_741_154));
        assert_eq!(sb_epoch_time(NOW, i32::MAX, -1), None);
        assert_eq!(sb_epoch_time(i64::MAX, 101, 100), Some(i64::MAX));
    }

    /// The payments go by proposal hash descending in `UintToArith256` order, which reads the
    /// last internal byte first: memcmp order would put `hash(1, 0xff)` above `hash(2, 0)`.
    #[test]
    fn sort_by_arith_hash() {
        let p = params(Network::Regtest);
        let (h, _) = window_start(&p);
        let (s, e) = around(&p, h);
        let addr = address(&p, 1);
        let (a, b, c) = (hash(1, 0xff), hash(2, 0x00), hash(1, 0x01));
        assert_eq!(a.cmp(&b), Ordering::Greater);
        assert_eq!(arith_cmp(&a, &b), Ordering::Less);
        let list = [
            (a, proposal(&addr, "1", s, e)),
            (c, proposal(&addr, "1", s, e)),
            (b, proposal(&addr, "1", s, e)),
        ];
        assert_eq!(paid(&plan(&p, h, open(), &list, NOW).unwrap()), vec![b, a, c]);
    }

    /// `GetHexStrData()` and `GetDataHash()` of a two-payment trigger. Pinned with values worked
    /// out from Core's code (`superblock.cpp:389-413`, `object.cpp:543-549`; base58check,
    /// `ValueFromAmount`, `uint256::ToString`, SHA256d over a serialized string) by an
    /// independent script, since the G5 recording does not exist yet.
    #[test]
    fn hex_str_data_and_data_hash() {
        let p = params(Network::Regtest);
        let mut h1 = [0u8; 32];
        for (i, b) in h1.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut h2 = [0xffu8; 32];
        h2[31] = 0x01;
        let sb = Superblock {
            height: 4500,
            payments: vec![
                Payment {
                    dest: Destination::PubKeyHash([0x11; 20]),
                    amount: 500_000_000,
                    proposal_hash: h1,
                },
                Payment {
                    dest: Destination::ScriptHash([0x22; 20]),
                    amount: 1,
                    proposal_hash: h2,
                },
            ],
            status: Status::Valid,
        };
        let json = "{\"event_block_height\": 4500, \"payment_addresses\": \"yMsgnH1xKGa85n4bq2imrZbG2KgrmGttAV|8hXwaDEiKTCZiRjbGZWN9Q9Bmz8WtQiJQm\", \"payment_amounts\": \"5.00000000|0.00000001\", \"proposal_hashes\": \"1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100|01ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\", \"type\":2}";
        let hex: String = json.bytes().map(|b| format!("{b:02x}")).collect();
        assert_eq!(sb.hex_str_data(&p), hex);
        let obj = trigger_template(&sb, 1_700_000_123, &p);
        assert_eq!(obj.data().as_slice(), json.as_bytes());
        let got: String = data_hash(&obj).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, "72dfcff1417add605ed3f168f7edd16300dffd78fb747dec64855c2538850190");
        assert_eq!(obj.hash_parent(), TxHash::from_lendian([0; 32]));
        assert_eq!(obj.collateral_hash(), TxHash::from_lendian([0; 32]));
        assert_eq!(obj.revision(), 1);
        assert_eq!(obj.time(), 1_700_000_123);
        assert_eq!(crate::object::object_type(&obj), crate::object::TRIGGER);
        assert_eq!(
            obj.masternode_outpoint(),
            OutPoint::new(TxHash::from_lendian([0; 32]), u32::MAX)
        );
        assert!(obj.sig().is_empty());
    }

    fn record(outcome: i32) -> BTreeMap<i32, VoteInstance> {
        BTreeMap::from([(
            SIGNAL_FUNDING,
            VoteInstance {
                outcome,
                last_update: 0,
                creation_time: 0,
            },
        )])
    }

    #[test]
    fn yes_decisions() {
        assert_eq!(yes_decision(None), YesDecision::Vote);
        let empty = BTreeMap::new();
        assert_eq!(yes_decision(Some(&empty)), YesDecision::Vote);
        let other =
            BTreeMap::from([(crate::vote::SIGNAL_VALID, record(OUTCOME_YES)[&SIGNAL_FUNDING])]);
        assert_eq!(yes_decision(Some(&other)), YesDecision::Vote);
        assert_eq!(
            yes_decision(Some(&record(OUTCOME_YES))),
            YesDecision::AlreadyVoted {
                yes: true
            }
        );
        // A NO record: no vote, and not the voted-yes trigger.
        assert_eq!(
            yes_decision(Some(&record(OUTCOME_NO))),
            YesDecision::AlreadyVoted {
                yes: false
            }
        );
    }

    #[test]
    fn no_vote_skips() {
        let p = params(Network::Regtest);
        let w = p.superblock_maturity_window();
        let (sb_height, cached, h) = (1000, 990, 1000 - w);
        let t = |last: u8, height: i32| ActiveTrigger {
            hash: hash(last, 0),
            height,
            known: true,
            our_record: None,
        };
        let plan = |triggers: &[ActiveTrigger<'_>], voted: Option<[u8; 32]>| {
            no_vote_plan(triggers, cached, h, voted, &p)
        };
        // The window edge: h == height - window votes, h + 1 short of it does not.
        assert_eq!(plan(&[t(1, sb_height)], None), vec![hash(1, 0)]);
        assert_eq!(plan(&[t(1, sb_height + 1)], None), Vec::<[u8; 32]>::new());
        // Outdated: at or below the cached height.
        assert_eq!(plan(&[t(1, cached)], None), Vec::<[u8; 32]>::new());
        assert_eq!(
            no_vote_plan(&[t(1, cached + 1)], cached, cached + 1 - w, None, &p),
            vec![hash(1, 0)]
        );
        // Unknown object.
        let unknown = ActiveTrigger {
            known: false,
            ..t(1, sb_height)
        };
        assert_eq!(plan(&[unknown], None), Vec::<[u8; 32]>::new());
        // The voted-yes trigger.
        assert_eq!(plan(&[t(1, sb_height), t(2, sb_height)], Some(hash(1, 0))), vec![hash(2, 0)]);
        // Our own FUNDING vote, either outcome.
        let (yes, no) = (record(OUTCOME_YES), record(OUTCOME_NO));
        for r in [&yes, &no] {
            let voted = ActiveTrigger {
                our_record: Some(r),
                ..t(1, sb_height)
            };
            assert_eq!(plan(&[voted, t(2, sb_height)], None), vec![hash(2, 0)]);
        }
        // A record without a FUNDING instance does not skip.
        let empty = BTreeMap::new();
        let other = ActiveTrigger {
            our_record: Some(&empty),
            ..t(1, sb_height)
        };
        assert_eq!(plan(&[other], None), vec![hash(1, 0)]);
        // Order follows the input.
        assert_eq!(plan(&[t(3, sb_height), t(1, sb_height)], None), vec![hash(3, 0), hash(1, 0)]);
    }

    /// D6: overflowing heights give no plan or no vote; overflowing epochs skip the proposal.
    #[test]
    fn overflows() {
        let p = params(Network::Regtest);
        let cycle = p.superblock_cycle();
        let top = i32::MAX - i32::MAX % cycle;
        assert_eq!(nearest_superblock_heights_checked(i32::MAX, &p), None);
        assert_eq!(nearest_superblock_heights_checked(top - 1, &p), Some((top - cycle, top)));

        // Epoch windows that overflow skip the proposal and the fill goes on.
        let (h, _) = window_start(&p);
        let (s, e) = around(&p, h);
        let addr = address(&p, 1);
        let list = [
            (hash(1, 0), proposal(&addr, "1", i64::MIN, e)),
            (hash(2, 0), proposal(&addr, "1", s, i64::MAX)),
            (hash(3, 0), proposal(&addr, "1", s, e)),
        ];
        assert_eq!(paid(&plan(&p, h, open(), &list, NOW).unwrap()), vec![hash(3, 0)]);
        // `i64::MIN + 7200` and `i64::MAX - 7200` still fit.
        let t = sb_epoch_time(NOW, nearest_superblock_heights(h, &p).1, h).unwrap();
        let edge = [(hash(1, 0), proposal(&addr, "1", i64::MIN + FUDGE_WINDOW, t))];
        assert_eq!(paid(&plan(&p, h, open(), &edge, NOW).unwrap()), vec![hash(1, 0)]);

        // A trigger height whose window start overflows gets no vote.
        let low = ActiveTrigger {
            hash: hash(1, 0),
            height: i32::MIN + 1,
            known: true,
            our_record: None,
        };
        assert!(no_vote_plan(&[low], i32::MIN, i32::MIN, None, &p).is_empty());
    }

    /// Reads Core does not catch come back as errors, after the budget check as in Core.
    #[test]
    fn uncaught_reads() {
        let p = params(Network::Regtest);
        let (h, next) = window_start(&p);
        let addr = address(&p, 1);
        let bad_epoch = format!(
            "{{\"end_epoch\":\"x\",\"payment_address\":\"{addr}\",\"payment_amount\":1,\"start_epoch\":1,\"type\":1}}"
        );
        let r = plan(&p, h, open(), &[(hash(1, 0), bad_epoch.clone().into_bytes())], NOW);
        assert_eq!(
            r,
            Err(ProposalReadError {
                proposal: hash(1, 0),
                what: "JSON value of type string is not of expected type number".to_owned(),
            })
        );
        // Over the budget, the epochs are never read.
        let budget = payments_limit(next, &p).unwrap();
        let over = bad_epoch.replace(
            "\"payment_amount\":1",
            &format!("\"payment_amount\":{}", crate::strings::value_from_amount(budget + 1)),
        );
        assert_eq!(plan(&p, h, open(), &[(hash(1, 0), over.into_bytes())], NOW), Ok(None));
    }
}
