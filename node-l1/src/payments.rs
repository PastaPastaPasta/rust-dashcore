//! EV3's `diff_block_value_payee`: the coinbase value and payee checks `ConnectBlock` makes
//! (`IsBlockValueValid`, `IsBlockPayeeValid` with `GetBlockTxOuts` and `CSuperblock::IsValid`)
//! against `oracle_block_value_payee` (v24; the entry dates from ABI 18, the pin is ABI 29).
//!
//! An input starts from a mined block of O2a-GV's payments fixture
//! (`dash-oracle-sys/tests/data/payments-v24.0.json.gz`: mainnet and testnet, budget windows, the
//! superblock start, superblocks before and after V20, each side of DIP3 enforcement, V20 and
//! testnet's MN_RR, the Classic, CreditPool and EvoReward eras, a banned list and a list with an
//! operator reward) and changes it: the node's
//! state (synced, governance loaded, the best ChainLock, V24, the superblock trigger), the fees,
//! the coinbase's outputs, the payee's operator reward, payout list and collateral shares, and the
//! trigger's payments.
//!
//! The node has no governance state (GV1a/GV1b, EV3 D3), so its side answers the governance
//! queries from Core's record (`IsSuperblockTriggered` and `GetSuperblockPayments`, from
//! [`governance_probe`] where the value check fails) and checks
//! every rule EV3 owns with them. Compared: the verdict, the check type, the era, V24's strict
//! multiplicity, the subsidy, both checks' answers and Core's value error, and the masternode
//! outputs `GetBlockTxOuts` computes.

use std::io::Read as _;
use std::sync::{Arc, OnceLock};

use dash_consensus::deployments::bip9_enabled;
use dash_consensus::payments::{MnRewardEra, PaymentParams};
use dash_consensus::primitives::read_compact_size;
use dash_consensus::subsidy::{COIN, block_subsidy_inner};
use dash_consensus::{Amount, Tx, decode_tx, money_range};
use dash_dml::codec::core_disk;
use dash_dml::payments::{
    BlockReward, BlockTxOuts, PayeeError, block_tx_outs, masternode_payee_valid,
};
use dash_dml::testkit::replay::{payee_rules, testnet};
use dash_dml::testkit::{fresh, list_disk_bytes};
use dash_dml::{Dmn, MnCold, MnList, OperatorKey};
use dash_evo::payments::{
    BAD_CB_AMOUNT, BAD_CB_PAYEE, BlockPayee, BlockValue, Ev3Branch, GovernanceView, PaymentsError,
    PaymentsParams, SuperblockCheck, block_payee_valid, block_value_valid,
    invalid_superblock_error, superblock_payee_valid, valid_superblock,
};
use dash_oracle_sys::payments::{VOTE_OUTCOME_YES, VOTE_SIGNAL_FUNDING};
use dash_oracle_sys::{
    BlockValuePayee, Chain, Checkpoints, CoinsView, Context, DEPLOYMENT_V24, ErrorKind, GovVote,
    MnList as CoreList, Oracle, PaymentsContext, Pin, ResultCode, RewardEra,
    SuperblockCheck as CoreCheck, ThresholdState, Trigger, Verdict,
};
use dash_params::{ChainParams, DeploymentPos};
use dash_pkc::bls::BlsSchemeKind;
use dash_primitives::{CollateralShare, NetInfo, NetInfoV2, PayoutShare, Transaction, TxOut};
use serde_json::Value;

use crate::dml::{mainnet, unhex};
use crate::{Compared, Divergence, Outcome, check, divergence};

/// The most edits an input makes.
pub const MAX_OPS: usize = 16;

/// `BlockValidationResult::BLOCK_RESULT_UNSET`: the payment rejects do not punish.
const BLOCK_RESULT_UNSET: ResultCode = ResultCode::Block(0);

/// The fixture, as committed by O2a-GV.
const FIXTURE: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../dash-oracle-sys/tests/data/payments-v24.0.json.gz");

/// A network's parameters, as each side reads them.
pub struct Net {
    pub params: &'static ChainParams,
    pub chain: fn() -> Chain,
    pub(crate) payments: PaymentsParams,
    pub(crate) payment: PaymentParams,
}

impl Net {
    fn new(params: &'static ChainParams, chain: fn() -> Chain) -> Option<Self> {
        Some(Self {
            params,
            chain,
            payments: PaymentsParams::from_chain_params(params).ok()?,
            payment: PaymentParams::from_chain_params(params).ok()?,
        })
    }
}

/// Mainnet and testnet.
pub(crate) fn nets() -> &'static [Net; 2] {
    static NETS: OnceLock<[Net; 2]> = OnceLock::new();
    NETS.get_or_init(|| {
        #[allow(clippy::expect_used)] // Core's parameters, constants, always build
        [
            Net::new(mainnet(), Chain::main).expect("mainnet's payment parameters"),
            Net::new(testnet(), Chain::test).expect("testnet's payment parameters"),
        ]
    })
}

/// A mined block of the fixture and the context its sample records.
pub struct Sample {
    pub name: String,
    pub net: &'static Net,
    /// The block's height.
    pub height: i32,
    /// The header and the transaction count, then the coinbase, then the other transactions.
    head: Vec<u8>,
    pub coinbase: Transaction,
    tail: Vec<u8>,
    pub fee: Amount,
    tip_header: [u8; 80],
    /// The parent's list as evodb stores it; none before DIP0003.
    pub prev_list: Option<Vec<u8>>,
    adjusted_time: i64,
    bls_legacy: bool,
    /// Each coinbase output's address, where it has one.
    pub addresses: Vec<Option<String>>,
    /// The collateral outpoints of the masternodes that fund the superblock trigger.
    voters: Vec<([u8; 32], u32)>,
    /// Whether the height is a superblock height (mainnet's coinbase paid a trigger; testnet's
    /// paid an empty budget).
    superblock: bool,
}

impl Sample {
    fn load(s: &Value) -> Option<Sample> {
        let net = match s["chain"].as_str()? {
            "main" => &nets()[0],
            "test" => &nets()[1],
            _ => return None,
        };
        let block = unhex(s["block"].as_str()?)?;
        let mut rest = block.get(80..)?;
        read_compact_size(&mut rest, true).ok()?;
        let head = block[..block.len() - rest.len()].to_vec();
        let decoded = decode_tx(&mut rest).ok()?;
        let coinbase = decoded.value().to_mutable();
        let voters = s["voters"]
            .as_array()?
            .iter()
            .map(|v| {
                let vout = u32::try_from(v[1].as_u64()?).ok()?;
                Some((unhex(v[0].as_str()?)?.try_into().ok()?, vout))
            })
            .collect::<Option<_>>()?;
        Some(Sample {
            name: s["name"].as_str()?.to_owned(),
            net,
            height: i32::try_from(s["height"].as_i64()?).ok()?,
            head,
            coinbase,
            tail: rest.to_vec(),
            fee: s["fee"].as_i64()?,
            tip_header: unhex(s["tip_header"].as_str()?)?.try_into().ok()?,
            prev_list: match &s["prev_list"] {
                Value::Null => None,
                list => Some(unhex(list.as_str()?)?),
            },
            adjusted_time: s["adjusted_time"].as_i64()?,
            bls_legacy: s["bls_legacy"].as_bool()?,
            addresses: s["addresses"]
                .as_array()?
                .iter()
                .map(|a| a.as_str().map(str::to_owned))
                .collect(),
            voters,
            superblock: s["superblock"].as_bool()?,
        })
    }

    /// The block with `coinbase` in place of the mined one (the merkle root is not checked).
    pub fn block_with(&self, coinbase: &Transaction) -> Vec<u8> {
        let mut block = self.head.clone();
        block.extend(dash_consensus::primitives::core_serialize(coinbase));
        block.extend(&self.tail);
        block
    }

    /// The oracle's context: the tip's header, V24's state after it and the parent's list.
    pub fn context(&self, v24: ThresholdState, list: Option<Vec<u8>>) -> Context {
        Context::new(
            self.height - 1,
            vec![self.tip_header],
            self.bls_legacy,
            vec![(DEPLOYMENT_V24, v24)],
            self.adjusted_time,
            Checkpoints::Disabled,
            CoinsView::Unchecked(vec![]),
            list.map_or(CoreList::Absent, CoreList::Supplied),
            vec![],
        )
    }

    /// Not synced, governance loaded, no triggers: a node in initial sync.
    pub fn ibd(&self) -> PaymentsContext {
        PaymentsContext {
            fee_reward: self.fee,
            synced: false,
            best_chainlock_height: None,
            governance_loaded: true,
            triggers: vec![],
        }
    }

    /// The parent's `nBits`.
    fn prev_bits(&self) -> u32 {
        u32::from_le_bytes([
            self.tip_header[72],
            self.tip_header[73],
            self.tip_header[74],
            self.tip_header[75],
        ])
    }
}

/// The fixture's payment samples.
#[allow(clippy::expect_used)] // the committed fixture
pub fn samples() -> &'static [Sample] {
    static SAMPLES: OnceLock<Vec<Sample>> = OnceLock::new();
    SAMPLES.get_or_init(|| {
        fixture()["payments"]
            .as_array()
            .expect("payment samples")
            .iter()
            .map(|s| Sample::load(s).expect("a payment sample"))
            .collect()
    })
}

/// The fixture's JSON.
#[allow(clippy::expect_used)] // the committed fixture
pub(crate) fn fixture() -> Value {
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(FIXTURE).expect(FIXTURE))
        .read_to_string(&mut text)
        .expect(FIXTURE);
    serde_json::from_str(&text).expect(FIXTURE)
}

/// A superblock trigger's payments: (address, amount).
pub type Payments = Vec<(String, Amount)>;

/// Each sample's trigger, at the superblock samples: the coinbase's outputs but the miner's and the
/// masternode payment, in coinbase order, as O2a-GV's tests build it.
pub fn triggers() -> &'static [Option<Payments>] {
    static TRIGGERS: OnceLock<Vec<Option<Payments>>> = OnceLock::new();
    TRIGGERS.get_or_init(|| samples().iter().map(trigger_of).collect())
}

fn trigger_of(s: &Sample) -> Option<Payments> {
    if !s.superblock {
        return None;
    }
    let block = s.block_with(&s.coinbase);
    let ctx = s.context(ThresholdState::Defined, s.prev_list.clone());
    let r =
        Oracle::new(Pin::V24_0).block_value_payee(&(s.net.chain)(), &block, &ctx, &s.ibd()).ok()?;
    let outs = &s.coinbase.outputs;
    let mut used: Vec<usize> = vec![];
    for expected in &r.masternode {
        used.push((0..outs.len()).find(|&i| {
            !used.contains(&i)
                && outs[i].value == expected.value
                && outs[i].script_pubkey.as_bytes() == expected.script_pubkey.as_slice()
        })?);
    }
    let paid: Amount = used.iter().map(|&i| outs[i].value).sum();
    let miner_value = r.block_subsidy + s.fee - paid;
    let miner = (0..outs.len()).find(|&i| outs[i].value == miner_value && !used.contains(&i))?;
    (0..outs.len())
        .filter(|i| *i != miner && !used.contains(i))
        .map(|i| Some((s.addresses.get(i)?.clone()?, outs[i].value)))
        .collect()
}

/// The sample named `name`'s index.
pub fn sample_index(name: &str) -> Option<usize> {
    samples().iter().position(|s| s.name == name)
}

/// Superblock sample `index` as mined, with the trigger its coinbase pays (unset in the case).
/// Testnet's superblock paid an empty budget, so its coinbase gains one payment of 1 DASH to the
/// miner's address, which the trigger pays. `None` off the superblock samples.
pub fn superblock_case(index: usize) -> Option<(Case, Payments)> {
    let mut case = Case::mined(index).ok()?;
    let mut payments = triggers().get(index)?.clone()?;
    if payments.is_empty() {
        let miner = case.sample().addresses.first()?.clone()?;
        let paid = case.coinbase.outputs.first()?.clone();
        payments.push((miner, COIN));
        case.coinbase.outputs.push(TxOut {
            value: COIN,
            ..paid
        });
    }
    Some((case, payments))
}

/// One comparison: a sample with its changes.
pub struct Case {
    pub sample: usize,
    pub coinbase: Transaction,
    /// The parent's list, our side's copy, when the sample has one and the input keeps it.
    pub list: Option<MnList>,
    /// The list's bytes for Core: the sample's own, or the changed list as evodb stores it.
    pub list_bytes: Option<Vec<u8>>,
    pub pay: PaymentsContext,
    /// V24's state after the tip, as the context seeds it.
    pub v24: ThresholdState,
    /// The trigger's payments, when the input has one.
    pub trigger: Option<Payments>,
}

impl Case {
    /// Sample `index` as mined, in initial sync.
    pub fn mined(index: usize) -> Result<Case, Divergence> {
        let s = &samples()[index];
        let list = match &s.prev_list {
            Some(bytes) => Some(
                core_disk::decode_list_s3(&mut &bytes[..])
                    .map_err(|_| divergence("evodb list read"))?,
            ),
            None => None,
        };
        Ok(Case {
            sample: index,
            coinbase: s.coinbase.clone(),
            list,
            list_bytes: s.prev_list.clone(),
            pay: s.ibd(),
            v24: ThresholdState::Defined,
            trigger: None,
        })
    }

    pub fn sample(&self) -> &'static Sample {
        &samples()[self.sample]
    }

    /// The block's height.
    pub fn sample_height(&self) -> i32 {
        self.sample().height
    }

    /// The payments context, with the trigger funded by every voter.
    fn payments_context(&self) -> PaymentsContext {
        self.payments_context_at(self.sample().height)
    }

    /// [`Case::payments_context`] with the trigger's `event_block_height` at `event_height`.
    pub(crate) fn payments_context_at(&self, event_height: i32) -> PaymentsContext {
        let s = self.sample();
        let mut pay = self.pay.clone();
        if let Some(payments) = &self.trigger {
            let votes = s
                .voters
                .iter()
                .map(|&(collateral_txid, collateral_vout)| GovVote {
                    collateral_txid,
                    collateral_vout,
                    signal: VOTE_SIGNAL_FUNDING,
                    outcome: VOTE_OUTCOME_YES,
                })
                .collect();
            pay.triggers.push(Trigger::superblock(event_height, payments, votes));
        }
        pay
    }

    /// The list, to edit, and its payee by `GetMNPayee`.
    fn payee_mut(&mut self) -> Option<(&mut MnList, Arc<Dmn>)> {
        let s = self.sample();
        let rules = payee_rules(s.net.params, s.height - 1);
        let list = self.list.as_mut()?;
        let payee = list.payee(rules)?.clone();
        Some((list, payee))
    }

    /// Re-encodes the edited list for Core.
    fn list_changed(&mut self) {
        self.list_bytes = self.list.as_ref().map(list_disk_bytes);
    }
}

/// What the node's side answers.
struct Ours {
    reject: Option<&'static str>,
    check: SuperblockCheck,
    era: MnRewardEra,
    strict: bool,
    block_subsidy: Amount,
    value_error: Option<String>,
    payee_valid: Option<bool>,
    /// `GetBlockTxOuts`, where the shim reports it.
    outputs: Option<BlockTxOuts>,
}

/// Why our side has no answer to compare.
enum NoAnswer {
    /// The payee check needs the list and the context has none: Core refuses the call.
    NeedsList,
    /// A node fault where Core asserts, or a gap Core's `int` has.
    Fault,
    /// Our side needs a governance answer Core did not give (see [`GovernanceGap`]).
    Governance(GovernanceGap),
}

impl From<PayeeError> for NoAnswer {
    fn from(_: PayeeError) -> Self {
        NoAnswer::Fault
    }
}

impl From<PaymentsError<GovernanceGap>> for NoAnswer {
    fn from(e: PaymentsError<GovernanceGap>) -> Self {
        match e {
            PaymentsError::Governance(gap) => NoAnswer::Governance(gap),
            PaymentsError::Subsidy(_) => NoAnswer::Fault,
        }
    }
}

/// A governance answer our side asked for and Core's records do not hold.
enum GovernanceGap {
    /// Core refuses the probe ([`governance_probe`]): the payee state is outside its domain,
    /// which the case's own call did not reach because its value check failed first.
    Refused,
    /// The record that would hold it did not compute it: the context has no list and DIP0003 is
    /// active at the tip (O2a-GV D16). Core needed no answer there, so ours asked where Core
    /// does not.
    NotRecorded,
    /// The probe diverged.
    Probe(Divergence),
}

/// Core's governance answers (GV1b's state, which the node does not have yet): whether a
/// superblock was triggered at the height and the best superblock's payments. They come from
/// the case's record when its value check passed, else from [`governance_probe`], called the
/// first time our side asks.
struct Recorded<'a> {
    case: &'a Case,
    record: Option<&'a BlockValuePayee>,
    probe: Option<BlockValuePayee>,
}

impl Recorded<'_> {
    fn answers(&mut self) -> Result<&BlockValuePayee, GovernanceGap> {
        if self.record.is_none() && self.probe.is_none() {
            self.probe = Some(match governance_probe(self.case) {
                Ok(Some(probe)) => probe,
                Ok(None) => return Err(GovernanceGap::Refused),
                Err(d) => return Err(GovernanceGap::Probe(d)),
            });
        }
        match self.record.or(self.probe.as_ref()) {
            Some(r) if r.outputs_computed => Ok(r),
            _ => Err(GovernanceGap::NotRecorded),
        }
    }
}

impl GovernanceView for Recorded<'_> {
    type Error = GovernanceGap;

    fn gov_valid(&mut self) -> Result<bool, Self::Error> {
        Ok(self.case.pay.governance_loaded)
    }

    fn sb_triggered(&mut self, _: u32) -> Result<bool, Self::Error> {
        Ok(self.answers()?.triggered)
    }

    fn sb_payments(&mut self, _: u32) -> Result<Option<Vec<TxOut>>, Self::Error> {
        let superblock = &self.answers()?.superblock;
        Ok((!superblock.is_empty()).then(|| {
            superblock
                .iter()
                .map(|o| TxOut {
                    value: o.value,
                    script_pubkey: o.script_pubkey.clone().into(),
                })
                .collect()
        }))
    }
}

/// The node's checks on `case`, with Core's governance answers from `core` (Core's record of the
/// case) or the probe.
fn ours(case: &Case, core: &BlockValuePayee) -> Result<Ours, NoAnswer> {
    let s = case.sample();
    let net = s.net;
    let c = &net.params.consensus;
    let h = s.height;
    let height = u32::try_from(h).map_err(|_| NoAnswer::Fault)?;
    let at = |deployment: u32| i64::from(h) >= i64::from(deployment);
    let fees = case.pay.fee_reward;

    // ConnectBlock's arguments (validation.cpp:2840-2865).
    let subsidy = block_subsidy_inner(
        s.prev_bits(),
        h - 1,
        at(c.v20_height.to_u32()),
        net.payments.subsidy(),
    )
    .map_err(|_| NoAnswer::Fault)?;
    let era = MnRewardEra::after(at(c.v20_height.to_u32()), at(c.mn_rr_height.to_u32()));
    let v24 = case.v24 == ThresholdState::Active && bip9_enabled(net.params, DeploymentPos::V24);
    let locked = case.pay.best_chainlock_height.is_some_and(|cl| cl >= h);
    let check = SuperblockCheck::for_block(!locked && case.pay.synced, v24);
    let rules = payee_rules(net.params, h - 1);
    let coinbase = Tx::new(case.coinbase.clone());
    let mut gov = Recorded {
        case,
        record: core.value_valid.then_some(core),
        probe: None,
    };
    let pp = &net.payments;
    let superblock = |gov: &mut Recorded, block_reward, is_v24| {
        valid_superblock(&coinbase, height, block_reward, is_v24, pp, gov)
    };

    let value = match block_value_valid(&coinbase, height, subsidy + fees, check, pp, &mut gov)? {
        BlockValue::Valid => None,
        BlockValue::Invalid(why) => Some(why),
        BlockValue::Ev3(Ev3Branch::ValidSuperblock {
            block_reward,
            is_v24,
        }) => {
            (!superblock(&mut gov, block_reward, is_v24)?).then(|| invalid_superblock_error(height))
        }
        BlockValue::Ev3(Ev3Branch::MasternodePayee {
            ..
        }) => return Err(NoAnswer::Fault),
    };
    let mut ours = Ours {
        reject: Some(BAD_CB_AMOUNT),
        check,
        era,
        strict: v24,
        block_subsidy: subsidy,
        value_error: value,
        payee_valid: None,
        outputs: None,
    };
    if ours.value_error.is_some() {
        return Ok(ours);
    }

    let reward = BlockReward {
        height: h,
        block_subsidy: subsidy,
        fee_reward: fees,
        era,
    };
    let payee_valid = match block_payee_valid(height, subsidy, fees, era, v24, check, pp, &mut gov)
        .map_err(NoAnswer::Governance)?
    {
        BlockPayee::Valid => true,
        BlockPayee::Ev3(Ev3Branch::MasternodePayee {
            block_subsidy,
            fee_reward,
            era,
            strict_multiplicity,
        }) => {
            let list = case.list.as_ref().ok_or(NoAnswer::NeedsList)?;
            let reward = BlockReward {
                height: h,
                block_subsidy,
                fee_reward,
                era,
            };
            masternode_payee_valid(
                &coinbase,
                list,
                rules,
                &reward,
                strict_multiplicity,
                &net.payment,
            )? && match superblock_payee_valid(height, subsidy, fees, check, pp, &mut gov)
                .map_err(NoAnswer::Governance)?
            {
                BlockPayee::Valid => true,
                BlockPayee::Ev3(Ev3Branch::ValidSuperblock {
                    block_reward,
                    is_v24,
                }) => superblock(&mut gov, block_reward, is_v24)?,
                BlockPayee::Ev3(Ev3Branch::MasternodePayee {
                    ..
                }) => return Err(NoAnswer::Fault),
            }
        }
        BlockPayee::Ev3(Ev3Branch::ValidSuperblock {
            block_reward,
            is_v24,
        }) => superblock(&mut gov, block_reward, is_v24)?,
    };
    ours.payee_valid = Some(payee_valid);
    ours.reject = (!payee_valid).then_some(BAD_CB_PAYEE);

    // The shim's outputs (oracle.h): FillBlockPayments' `GetBlockTxOuts` on the parent's list, or
    // on no list before DIP0003 is active at the tip.
    let dip3_at_tip = i64::from(h - 1) >= i64::from(c.dip0003_height.to_u32());
    let empty = MnList::default_pre_dip3();
    let list = match &case.list {
        Some(list) => Some(list),
        None if !dip3_at_tip => Some(&empty),
        None => None,
    };
    if let Some(list) = list {
        ours.outputs = Some(block_tx_outs(list, rules, &reward, &net.payment)?);
    }
    Ok(ours)
}

fn core_check(check: CoreCheck) -> SuperblockCheck {
    match check {
        CoreCheck::NoCheck => SuperblockCheck::NoCheck,
        CoreCheck::AllowDuplicates => SuperblockCheck::AllowDuplicates,
        CoreCheck::DisallowDuplicates => SuperblockCheck::DisallowDuplicates,
    }
}

fn core_era(era: RewardEra) -> MnRewardEra {
    match era {
        RewardEra::Classic => MnRewardEra::Classic,
        RewardEra::CreditPool => MnRewardEra::CreditPool,
        RewardEra::EvoReward => MnRewardEra::EvoReward,
    }
}

/// Whether Core's outputs are ours.
fn same_outputs(core: &[dash_oracle_sys::TxOut], ours: &[TxOut]) -> bool {
    core.len() == ours.len()
        && core.iter().zip(ours).all(|(c, o)| {
            c.value == o.value && c.script_pubkey.as_slice() == o.script_pubkey.as_bytes()
        })
}

/// Whether `GetValueOut` and `CheckTransaction`'s money checks pass: Core never reaches the
/// payment checks otherwise.
fn coinbase_in_range(tx: &Transaction) -> bool {
    let mut total: Amount = 0;
    tx.outputs.iter().all(|o| {
        total = total.saturating_add(o.value);
        money_range(o.value) && money_range(total)
    })
}

/// One case on both sides.
pub fn compare_case(case: &Case) -> Compared {
    compare_case_record(case).map(|(outcome, _)| outcome)
}

/// Core's governance answers on `case`'s lists and triggers (`IsSuperblockTriggered`,
/// `GetSuperblockPayments`): a record of the coinbase paying nothing, unsynced, whose value
/// check passes. Core records the answers only where it does (O2a-GV D19). `None` where Core
/// refuses the call.
pub fn governance_probe(case: &Case) -> Result<Option<BlockValuePayee>, Divergence> {
    let s = case.sample();
    let mut unpaid = case.coinbase.clone();
    unpaid.outputs.iter_mut().for_each(|o| o.value = 0);
    let pay = PaymentsContext {
        synced: false,
        ..case.payments_context()
    };
    match Oracle::new(Pin::V24_0).block_value_payee(
        &(s.net.chain)(),
        &s.block_with(&unpaid),
        &s.context(case.v24, case.list_bytes.clone()),
        &pay,
    ) {
        Ok(probe) if probe.value_valid => Ok(Some(probe)),
        Ok(_) => Err(divergence("governance probe")),
        Err(e) if matches!(e.kind, ErrorKind::Context | ErrorKind::Argument) => Ok(None),
        Err(_) => Err(divergence("oracle error")),
    }
}

/// [`compare_case`], with Core's record when it has one.
pub fn compare_case_record(case: &Case) -> Result<(Outcome, Option<BlockValuePayee>), Divergence> {
    let s = case.sample();
    if !coinbase_in_range(&case.coinbase) || !money_range(case.pay.fee_reward) {
        return Ok((Outcome::OutOfDomain, None));
    }
    let block = s.block_with(&case.coinbase);
    let ctx = s.context(case.v24, case.list_bytes.clone());
    let core = match Oracle::new(Pin::V24_0).block_value_payee(
        &(s.net.chain)(),
        &block,
        &ctx,
        &case.payments_context(),
    ) {
        Ok(core) => core,
        Err(e) if matches!(e.kind, ErrorKind::Context | ErrorKind::Argument) => {
            return Ok((Outcome::OutOfDomain, None));
        }
        Err(_) => return Err(divergence("oracle error")),
    };
    let ours = match ours(case, &core) {
        Ok(ours) => ours,
        Err(NoAnswer::NeedsList) => return Err(divergence("list needed")),
        Err(NoAnswer::Fault) => return Err(divergence("node fault")),
        Err(NoAnswer::Governance(GovernanceGap::Refused)) => {
            return Ok((Outcome::OutOfDomain, None));
        }
        Err(NoAnswer::Governance(GovernanceGap::NotRecorded)) => {
            return Err(divergence("governance not recorded"));
        }
        Err(NoAnswer::Governance(GovernanceGap::Probe(d))) => return Err(d),
    };

    match (&core.verdict, ours.reject) {
        (Verdict::Accept, None) => {}
        (Verdict::Reject(rej), Some(reason)) => {
            check(rej.reason == reason, "reject reason")?;
            check(rej.result == BLOCK_RESULT_UNSET, "reject result")?;
        }
        _ => return Err(divergence("verdict")),
    }
    check(core_check(core.check) == ours.check, "check type")?;
    check(core_era(core.era) == ours.era, "era")?;
    check(core.strict == ours.strict, "strict multiplicity")?;
    check(core.block_subsidy == ours.block_subsidy, "block subsidy")?;
    check(core.value_valid == ours.value_error.is_none(), "value valid")?;
    check(core.value_error == ours.value_error.unwrap_or_default(), "value error")?;
    check(core.payee_valid == ours.payee_valid, "payee valid")?;
    check(core.outputs_computed == ours.outputs.is_some(), "outputs computed")?;
    if let Some(owed) = &ours.outputs {
        check(core.txouts_ok == owed.computed, "txouts ok")?;
        check(same_outputs(&core.masternode, &owed.outputs), "masternode outputs")?;
    }
    let outcome = if ours.reject.is_none() {
        Outcome::BothAccept
    } else {
        Outcome::BothReject
    };
    Ok((outcome, Some(core)))
}

/// A script `tag` makes: a P2PKH to `[tag; 20]`.
fn script(tag: u8) -> Vec<u8> {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend([tag; 20]);
    script.extend([0x88, 0xac]);
    script
}

/// Operator rewards an edit sets, in basis points.
const OPERATOR_REWARDS: [u16; 6] = [0, 1, 1_234, 5_000, 9_999, 10_000];

/// `ProTxVersion::ExtAddr`, the state version with payout lists and collateral shares.
const EXT_ADDR: i32 = 3;

/// `MasternodePayoutShare::MAX_REWARD` (evo/providertx.h:35): a whole payout, in basis points.
const MAX_PAYOUT_REWARD: u16 = 10_000;

/// Payout and share sizes an edit picks from.
const PAYOUT_BPS: [u16; 8] = [0, 1, 2_500, 3_333, 5_000, 6_667, 9_999, 10_000];
const SHARE_AMOUNTS: [Amount; 6] = [0, 1, 100 * COIN, 300 * COIN, 600 * COIN, 1_000 * COIN];

/// The case an input describes: `[sample, flags, chainlock, fee delta (2, LE), (op, a, b)*]`.
///
/// Flags: synced (bit 0), governance loaded (1), a ChainLock at height − 2 + `chainlock % 4` (2),
/// V24 active after the tip (3; Core ignores it on mainnet, where V24 is never active), the
/// sample's trigger (4; superblock samples only), no list in the context (5).
pub fn case_from_bytes(input: &[u8]) -> Option<Result<Case, Divergence>> {
    let (&[selector, flags, chainlock, fee_lo, fee_hi], ops) = input.split_first_chunk::<5>()?;
    let index = usize::from(selector) % samples().len();
    let mut case = match Case::mined(index) {
        Ok(case) => case,
        Err(e) => return Some(Err(e)),
    };
    let h = case.sample().height;
    case.pay.synced = flags & 1 != 0;
    case.pay.governance_loaded = flags & 2 != 0;
    if flags & 4 != 0 {
        case.pay.best_chainlock_height = Some(h - 2 + i32::from(chainlock % 4));
    }
    if flags & 8 != 0 {
        case.v24 = ThresholdState::Active;
    }
    if flags & 16 != 0 {
        case.trigger = triggers()[index].clone();
    }
    if flags & 32 != 0 {
        case.list = None;
        case.list_bytes = None;
    }
    case.pay.fee_reward += Amount::from(i16::from_le_bytes([fee_lo, fee_hi]));

    for &[op, a, b] in ops.as_chunks::<3>().0.iter().take(MAX_OPS) {
        edit(&mut case, op, a, b);
    }
    Some(Ok(case))
}

/// Applies one edit; an edit that does not apply to the case does nothing.
fn edit(case: &mut Case, op: u8, a: u8, b: u8) {
    match op % 15 {
        8 => return edit_operator(case, a, b),
        9 => return edit_payouts(case, a, b),
        10 => return edit_shares(case, a, b),
        11 => return ban_payee(case),
        trigger @ 12..=14 => return edit_trigger(case, trigger, a, b),
        _ => {}
    }
    let outs = &mut case.coinbase.outputs;
    let n = outs.len();
    let (i, j) = (usize::from(a) % n.max(1), usize::from(b) % n.max(1));
    let delta = Amount::from(b) + 1;
    match op % 15 {
        0 if n > 1 => {
            outs.remove(i);
        }
        1 if n > 0 => outs.push(outs[i].clone()),
        2 if n > 0 => outs.swap(i, j),
        3 if n > 0 => outs[i].value += delta,
        4 if n > 0 && outs[i].value >= delta => outs[i].value -= delta,
        5 if n > 1 && outs[i].value >= delta => {
            outs[i].value -= delta;
            outs[(i + 1) % n].value += delta;
        }
        6 if n > 0 => outs[i].script_pubkey = outs[j].script_pubkey.clone(),
        7 => outs.push(TxOut {
            value: Amount::from(b),
            script_pubkey: vec![0x6a].into(),
        }),
        _ => {}
    }
}

/// The payee's operator reward and operator payout script (registration fields: the masternode
/// is removed and added back with them).
fn edit_operator(case: &mut Case, a: u8, b: u8) {
    let Some((list, old)) = case.payee_mut() else {
        return;
    };
    let pro_tx = old.pro_tx_hash;
    let mut dmn = (*old).clone();
    dmn.operator_reward = OPERATOR_REWARDS[usize::from(a) % OPERATOR_REWARDS.len()];
    let mut cold = (*dmn.cold).clone();
    cold.script_operator_payout = if b & 1 == 0 {
        script(b)
    } else {
        Vec::new()
    };
    dmn.cold = Arc::new(fresh(cold));
    let mut edited = list.clone();
    if edited.remove(&pro_tx).is_ok() && edited.try_add(Arc::new(dmn), false).is_ok() {
        *list = edited;
        case.list_changed();
    }
}

/// The payee's owner payout list (made version 3): `a % 4 + 1` entries.
fn edit_payouts(case: &mut Case, a: u8, b: u8) {
    edit_payee_state(case, |cold| {
        to_ext_addr(cold);
        let first = cold.payouts.first().map(|p| p.script_payout.as_bytes().to_vec());
        cold.payouts = (0..usize::from(a % 4) + 1)
            .map(|k| {
                let pick = usize::from(b.rotate_left(k as u32 * 3));
                PayoutShare {
                    script_payout: match (pick % 3, &first) {
                        (0, Some(first)) => first.clone(),
                        _ => script(b.wrapping_add(k as u8)),
                    }
                    .into(),
                    reward: PAYOUT_BPS[pick % PAYOUT_BPS.len()],
                }
            })
            .collect();
        true
    });
}

/// The payee's collateral shares (made version 3): `a % 3 + 1` shares, some without a reward
/// script.
fn edit_shares(case: &mut Case, a: u8, b: u8) {
    edit_payee_state(case, |cold| {
        to_ext_addr(cold);
        cold.shares = (0..usize::from(a % 3) + 1)
            .map(|k| {
                let pick = usize::from(b.rotate_left(k as u32 * 2));
                CollateralShare {
                    amount: SHARE_AMOUNTS[pick % SHARE_AMOUNTS.len()],
                    script_refund: script(0x40 + k as u8).into(),
                    script_reward: if pick & 4 == 0 {
                        script(0x50 + k as u8)
                    } else {
                        Vec::new()
                    }
                    .into(),
                    key_id_owner: [0x60 + k as u8; 20].into(),
                }
            })
            .collect();
        true
    });
}

/// `cold` as an ExtAddr (version 3) state, if older: its payout script becomes a one-entry payout
/// list, its NetInfo an empty extended one (whose entries replace an evonode's platform ports)
/// and its operator key the same point in the basic scheme (the fixture's lists hold no
/// version-3 state).
fn to_ext_addr(cold: &mut MnCold) {
    if cold.version >= EXT_ADDR {
        return;
    }
    cold.version = EXT_ADDR;
    cold.pub_key_operator =
        OperatorKey::from_value(cold.pub_key_operator.get(), BlsSchemeKind::Basic);
    cold.payouts = vec![PayoutShare {
        script_payout: std::mem::take(&mut cold.script_payout).into(),
        reward: MAX_PAYOUT_REWARD,
    }];
    cold.platform_p2p_port = 0;
    cold.platform_http_port = 0;
    cold.net_info = Arc::new(NetInfo::Extended(NetInfoV2 {
        version: 1,
        entries: Vec::new(),
    }));
}

/// Changes the payee's state by `f` (when it returns true) through `UpdateMN`.
fn edit_payee_state(case: &mut Case, f: impl FnOnce(&mut MnCold) -> bool) {
    let Some((list, dmn)) = case.payee_mut() else {
        return;
    };
    let mut cold = (*dmn.cold).clone();
    if !f(&mut cold) {
        return;
    }
    if list.update(&dmn.pro_tx_hash, dmn.hot, Arc::new(fresh(cold))).is_ok() {
        case.list_changed();
    }
}

/// PoSe-bans the payee, so the next masternode in the queue is paid (or none is).
fn ban_payee(case: &mut Case) {
    let h = case.sample().height;
    let Some((list, dmn)) = case.payee_mut() else {
        return;
    };
    let mut hot = dmn.hot;
    hot.ban_if_not_banned(h - 1);
    if list.update_hot(&dmn.pro_tx_hash, hot).is_ok() {
        case.list_changed();
    }
}

/// The trigger's payment `a`: its amount changed by `b + 1` (op 12, up or down by `b`'s low
/// bit), removed (13) or listed twice (14).
fn edit_trigger(case: &mut Case, op: u8, a: u8, b: u8) {
    let Some(payments) = case.trigger.as_mut() else {
        return;
    };
    if payments.is_empty() {
        return;
    }
    let i = usize::from(a) % payments.len();
    let delta = Amount::from(b >> 1) + 1;
    match op {
        12 if b & 1 == 0 => payments[i].1 += delta,
        12 if payments[i].1 > delta => payments[i].1 -= delta,
        13 => {
            payments.remove(i);
        }
        14 => payments.insert(i + 1, payments[i].clone()),
        _ => {}
    }
}

/// `diff_block_value_payee` (see [`case_from_bytes`]).
pub fn compare_block_value_payee(input: &[u8], pin: Pin) -> Compared {
    if pin != Pin::V24_0 {
        return Ok(Outcome::OutOfDomain);
    }
    match case_from_bytes(input) {
        Some(case) => compare_case(&case?),
        None => Ok(Outcome::OutOfDomain),
    }
}

/// The target's start: the fixture and the triggers, read once.
pub fn start(_: Pin) {
    let _ = triggers();
}

/// The target's public seeds: every sample in every node state (each flag set, a ChainLock at
/// and below the height), and every edit at a few arguments on the synced, governance-loaded
/// sample, with and without V24 and the trigger. Call bytes only, judged by the oracle when run.
pub fn seed_inputs() -> Vec<Vec<u8>> {
    let mut seeds = Vec::new();
    for sample in 0..samples().len() as u8 {
        for flags in 0..64u8 {
            seeds.push(vec![sample, flags, 1, 0, 0]);
            if flags & 4 != 0 {
                seeds.push(vec![sample, flags, 2, 0, 0]);
            }
        }
        for flags in [0x03, 0x1b] {
            for op in 0..15u8 {
                for (a, b) in [(0, 0), (1, 1), (2, 7), (5, 254)] {
                    seeds.push(vec![sample, flags, 0, 0, 0, op, a, b]);
                }
            }
        }
    }
    seeds
}
