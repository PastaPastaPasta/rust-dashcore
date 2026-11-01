//! Block payments and the credit pool (ABI 18, v24 only; PLAN O2a-GV).
//!
//! [`Oracle::block_value_payee`] runs the coinbase checks `ConnectBlock()` makes with state
//! from outside the chain: the governance triggers, the sync and ChainLock gates and the fees.
//! [`Oracle::credit_pool_step`] runs one block of the credit pool. The formats are in
//! `oracle.h` ("block payments and the credit pool") and
//! `docs/node-program/designs/O2a-GV-context.md`.

use super::*;
use crate::context::{Context, UnlockIndexes};

/// `VOTE_SIGNAL_FUNDING`: the signal superblock triggers are counted on.
pub const VOTE_SIGNAL_FUNDING: u8 = 1;
/// `VOTE_OUTCOME_YES`.
pub const VOTE_OUTCOME_YES: u8 = 1;
/// `VOTE_OUTCOME_NO`.
pub const VOTE_OUTCOME_NO: u8 = 2;
/// `VOTE_OUTCOME_ABSTAIN`.
pub const VOTE_OUTCOME_ABSTAIN: u8 = 3;

/// A masternode's current vote on a governance object (`mapCurrentMNVotes`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GovVote {
    /// The voting masternode's collateral outpoint.
    pub collateral_txid: [u8; 32],
    pub collateral_vout: u32,
    /// `vote_signal_enum_t` (1-4).
    pub signal: u8,
    /// `vote_outcome_enum_t` (1-3).
    pub outcome: u8,
}

/// A governance object the node's `SuperblockManager` holds, with its votes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    /// A `CGovernanceObject` in network serialization (`Governance::Object`).
    pub object: Vec<u8>,
    /// At most one per collateral and signal.
    pub votes: Vec<GovVote>,
}

impl Trigger {
    /// A superblock TRIGGER object as a sentinel submits it (`CSuperblock::GetHexStrData()`'s
    /// JSON in `vchData`), paying `payments` (address, amount in duffs) at `event_height`, with
    /// `votes`. The other fields are fixed: the payment checks never read them.
    pub fn superblock(event_height: i32, payments: &[(String, i64)], votes: Vec<GovVote>) -> Self {
        let join = |parts: Vec<String>| parts.join("|");
        let addresses = join(payments.iter().map(|(address, _)| address.clone()).collect());
        let amounts = join(
            payments
                .iter()
                .map(|&(_, amount)| format!("{}.{:08}", amount / 100_000_000, amount % 100_000_000))
                .collect(),
        );
        let hashes = join((1..=payments.len()).map(|i| format!("{i:064x}")).collect());
        let data = format!(
            "{{\"event_block_height\": {event_height}, \"payment_addresses\": \"{addresses}\", \
             \"payment_amounts\": \"{amounts}\", \"proposal_hashes\": \"{hashes}\", \"type\":2}}"
        );
        let mut object = Vec::new();
        object.extend([0u8; 32]); // hashParent
        object.extend(1i32.to_le_bytes()); // revision
        object.extend(1_600_000_000i64.to_le_bytes()); // time
        object.extend([0u8; 32]); // collateralHash
        write_bytes(&mut object, data.as_bytes());
        object.extend(2i32.to_le_bytes()); // GovernanceObject::TRIGGER
        object.extend([0u8; 32]); // masternodeOutpoint
        object.extend(u32::MAX.to_le_bytes());
        write_bytes(&mut object, &[]); // vchSig
        Self {
            object,
            votes,
        }
    }
}

/// What the payment checks read besides the block and the chain context (oracle.h
/// "payments context"). No `Default`: every field can change the verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentsContext {
    /// The block's fees (`ConnectBlock()`'s `nFees`).
    pub fee_reward: i64,
    /// `CMasternodeSync::IsSynced()`.
    pub synced: bool,
    /// The best ChainLock's height, if any.
    pub best_chainlock_height: Option<i32>,
    /// `SuperblockManager::IsValid()`: the governance cache is loaded.
    pub governance_loaded: bool,
    pub triggers: Vec<Trigger>,
}

impl PaymentsContext {
    /// The shim's payments-context bytes (format 1).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = vec![1u8];
        out.extend(self.fee_reward.to_le_bytes());
        out.push(self.synced.into());
        out.extend(self.best_chainlock_height.unwrap_or(-1).to_le_bytes());
        out.push(self.governance_loaded.into());
        write_compact_size(&mut out, self.triggers.len() as u64);
        for trigger in &self.triggers {
            write_bytes(&mut out, &trigger.object);
            write_compact_size(&mut out, trigger.votes.len() as u64);
            for vote in &trigger.votes {
                out.extend(vote.collateral_txid);
                out.extend(vote.collateral_vout.to_le_bytes());
                out.extend([vote.signal, vote.outcome]);
            }
        }
        out
    }
}

/// `SuperBlockCheckType`: how far `ConnectBlock()` checks superblock payments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuperblockCheck {
    /// Chainlocked, or the node is not synced: superblock bounds only.
    NoCheck = 0,
    AllowDuplicates = 1,
    /// V24: each expected payment matches a distinct output.
    DisallowDuplicates = 2,
}

/// A `CTxOut`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOut {
    pub value: i64,
    pub script_pubkey: Vec<u8>,
}

/// Result of [`Oracle::block_value_payee`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockValuePayee {
    /// `ConnectBlock()`'s verdict: `bad-cb-amount`, else `bad-cb-payee`.
    pub verdict: Verdict,
    pub check: SuperblockCheck,
    pub era: RewardEra,
    /// `strict_multiplicity` (V24 active after the tip).
    pub strict: bool,
    /// `GetBlockSubsidy(pindex)`.
    pub block_subsidy: i64,
    /// `IsBlockValueValid()` and its `strErrorRet`.
    pub value_valid: bool,
    pub value_error: String,
    /// `IsBlockPayeeValid()`, `None` when `value_valid` is false: `ConnectBlock()` stops at
    /// `bad-cb-amount` (validation.cpp:2846-2849), so neither the payee check nor the payee
    /// reward refusals run.
    pub payee_valid: Option<bool>,
    /// Whether the four fields below were computed. They are not (false, empty) when
    /// `value_valid` is false, or when DIP0003 is active at the tip and the context holds no
    /// list ([`MnList::Absent`](crate::MnList::Absent)): only Core's own reads can need the
    /// list then.
    pub outputs_computed: bool,
    /// `GetBlockTxOuts()`: false when the list has masternodes but no payee.
    pub txouts_ok: bool,
    /// Its outputs: the platform share, the owner payouts or share splits, the operator reward.
    pub masternode: Vec<TxOut>,
    /// `IsSuperblockTriggered()` over the tip's list.
    pub triggered: bool,
    /// `GetSuperblockPayments()` when triggered.
    pub superblock: Vec<TxOut>,
}

/// A `CCreditPool`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditPool {
    /// `getcreditpoolinfo`'s `balance`.
    pub locked: i64,
    /// Its `currentlimit`.
    pub current_limit: i64,
    /// Its `window.unlocked`.
    pub lately_unlocked: i64,
    pub indexes: UnlockIndexes,
}

impl CreditPool {
    fn write(&self, out: &mut Vec<u8>) {
        out.extend(self.locked.to_le_bytes());
        out.extend(self.current_limit.to_le_bytes());
        out.extend(self.lately_unlocked.to_le_bytes());
        self.indexes.write(out);
    }
}

/// What a credit-pool step reads besides the block and the chain context (oracle.h "step
/// context").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepContext {
    /// The pool after the tip (empty unless V20 is active at the tip).
    pub prev: CreditPool,
    /// Blocks of context headers on the node's disk besides the stepped one (network
    /// serialization): the window start `CreditPoolPeriodBlocks()` below it.
    pub blocks: Vec<Vec<u8>>,
}

impl StepContext {
    /// The shim's step-context bytes (format 1).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = vec![1u8];
        self.prev.write(&mut out);
        write_compact_size(&mut out, self.blocks.len() as u64);
        for block in &self.blocks {
            out.extend(block);
        }
        out
    }
}

/// Result of [`Oracle::credit_pool_step`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditPoolStep {
    /// `CheckCreditPoolDiffForBlock()`'s verdict (accept when it did not run).
    pub verdict: Verdict,
    /// Whether it ran: the coinbase is a CbTx.
    pub checked: bool,
    /// The balance the CbTx must carry, when DIP0008 and V20 are active, the diff exists and its
    /// total is an `int64` (ABI 29: rc.4's `GetTotalLocked()` throws otherwise).
    pub expected: Option<i64>,
    /// `GetCreditPool(block)`, when accepted.
    pub pool: Option<CreditPool>,
}

/// A reader over a record the shim wrote in Core's serialization.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn flag(&mut self) -> Option<bool> {
        match self.u8()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn len(&mut self) -> Option<usize> {
        usize::try_from(read_compact_size(&mut self.0)?).ok()
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.len()?;
        Some(self.take(n)?.to_vec())
    }
    fn txouts(&mut self) -> Option<Vec<TxOut>> {
        (0..self.len()?)
            .map(|_| {
                Some(TxOut {
                    value: self.i64()?,
                    script_pubkey: self.bytes()?,
                })
            })
            .collect()
    }
    fn pool(&mut self) -> Option<CreditPool> {
        Some(CreditPool {
            locked: self.i64()?,
            current_limit: self.i64()?,
            lately_unlocked: self.i64()?,
            indexes: UnlockIndexes {
                ranges: (0..self.len()?)
                    .map(|_| Some((self.u64()?, self.u64()?)))
                    .collect::<Option<_>>()?,
            },
        })
    }
}

fn parse_block_value_payee(bytes: &[u8], verdict: Verdict) -> Option<BlockValuePayee> {
    let mut r = Reader(bytes);
    let check = match r.u8()? {
        0 => SuperblockCheck::NoCheck,
        1 => SuperblockCheck::AllowDuplicates,
        2 => SuperblockCheck::DisallowDuplicates,
        _ => return None,
    };
    let era = match r.u8()? {
        0 => RewardEra::Classic,
        1 => RewardEra::CreditPool,
        2 => RewardEra::EvoReward,
        _ => return None,
    };
    let strict = r.flag()?;
    let block_subsidy = r.i64()?;
    let value_valid = r.flag()?;
    let value_error = String::from_utf8(r.bytes()?).ok()?;
    let payee_valid = r.flag()?;
    if !value_valid && payee_valid {
        return None;
    }
    let record = BlockValuePayee {
        verdict,
        check,
        era,
        strict,
        block_subsidy,
        value_valid,
        value_error,
        payee_valid: value_valid.then_some(payee_valid),
        outputs_computed: r.flag()?,
        txouts_ok: r.flag()?,
        masternode: r.txouts()?,
        triggered: r.flag()?,
        superblock: r.txouts()?,
    };
    // Outputs only after a passing value check, and none when not computed.
    let consistent = if record.outputs_computed {
        value_valid
    } else {
        !record.txouts_ok
            && !record.triggered
            && record.masternode.is_empty()
            && record.superblock.is_empty()
    };
    (r.0.is_empty() && consistent).then_some(record)
}

fn parse_credit_pool_step(bytes: &[u8], verdict: Verdict) -> Option<CreditPoolStep> {
    let mut r = Reader(bytes);
    let checked = r.flag()?;
    let has_expected = r.flag()?;
    let expected = r.i64()?;
    let has_pool = r.flag()?;
    let pool = r.pool()?;
    r.0.is_empty().then_some(CreditPoolStep {
        verdict,
        checked,
        expected: has_expected.then_some(expected),
        pool: has_pool.then_some(pool),
    })
}

pub(crate) fn malformed(entry: &str) -> Error {
    Error {
        kind: ErrorKind::Exception,
        message: format!("{entry}'s output record is malformed"),
    }
}

impl Oracle {
    /// `IsBlockValueValid()` and `IsBlockPayeeValid()` as `ConnectBlock()` calls them for `block`
    /// on top of the context's tip, with the outputs `FillBlockPayments()` would add there.
    pub fn block_value_payee(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        payments: &PaymentsContext,
    ) -> Result<BlockValuePayee> {
        self.block_value_payee_raw(chain, block, ctx, &payments.serialize())
    }

    /// [`Oracle::block_value_payee`] with the payments-context bytes of any format.
    pub(crate) fn block_value_payee_raw(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        pay: &[u8],
    ) -> Result<BlockValuePayee> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let mut state = ffi::OracleValidation::default();
            let mut accepted = false;
            let record = self.with_buffer(1024, |out, len| {
                let status = unsafe {
                    (self.api.block_value_payee)(
                        block.as_ptr(),
                        block.len(),
                        ctx.as_ptr(),
                        ctx.len(),
                        pay.as_ptr(),
                        pay.len(),
                        out,
                        len,
                        &mut state,
                    )
                };
                accepted = status == ffi::ORACLE_OK;
                status
            })?;
            parse_block_value_payee(&record, block_verdict(accepted, &state))
                .ok_or_else(|| malformed("oracle_block_value_payee"))
        })
    }

    /// `CheckCreditPoolDiffForBlock()` for `block` on top of the context's tip, then
    /// `GetCreditPool(block)`.
    pub fn credit_pool_step(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        step: &StepContext,
    ) -> Result<CreditPoolStep> {
        let ctx = ctx.serialize();
        let step = step.serialize();
        self.on_chain(chain, || {
            let mut state = ffi::OracleValidation::default();
            let mut accepted = false;
            let record = self.with_buffer(256, |out, len| {
                let status = unsafe {
                    (self.api.credit_pool_step)(
                        block.as_ptr(),
                        block.len(),
                        ctx.as_ptr(),
                        ctx.len(),
                        step.as_ptr(),
                        step.len(),
                        out,
                        len,
                        &mut state,
                    )
                };
                accepted = status == ffi::ORACLE_OK;
                status
            })?;
            parse_credit_pool_step(&record, block_verdict(accepted, &state))
                .ok_or_else(|| malformed("oracle_credit_pool_step"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_parse_exactly() {
        let mut record = vec![2u8, 1, 1];
        record.extend(5i64.to_le_bytes());
        // value_valid, value_error, payee_valid, outputs_computed, txouts_ok, one masternode output.
        record.extend([1, 2, b'n', b'o', 1, 1, 1, 1]);
        record.extend(7i64.to_le_bytes());
        record.extend([1, 0x6a, 0, 0]);
        let parsed = parse_block_value_payee(&record, Verdict::Accept).unwrap();
        assert_eq!((parsed.payee_valid, parsed.outputs_computed), (Some(true), true));
        assert_eq!(parsed.check, SuperblockCheck::DisallowDuplicates);
        assert_eq!(parsed.era, RewardEra::CreditPool);
        assert_eq!((parsed.block_subsidy, parsed.value_error.as_str()), (5, "no"));
        assert_eq!(
            parsed.masternode,
            vec![TxOut {
                value: 7,
                script_pubkey: vec![0x6a]
            }]
        );
        assert!(!parsed.triggered && parsed.superblock.is_empty());
        assert!(parse_block_value_payee(&record[..record.len() - 1], Verdict::Accept).is_none());
        assert!(
            parse_block_value_payee(&[record.as_slice(), &[0]].concat(), Verdict::Accept).is_none()
        );
        // A failed value check: no payee check, no outputs.
        let mut stopped = vec![2u8, 1, 1];
        stopped.extend(5i64.to_le_bytes());
        stopped.extend([0, 2, b'n', b'o', 0, 0, 0, 0, 0, 0]);
        let parsed = parse_block_value_payee(&stopped, Verdict::Accept).unwrap();
        assert_eq!((parsed.payee_valid, parsed.outputs_computed), (None, false));
        // Records the shim never writes: a payee verdict or outputs after a failed value check,
        // and outputs that are set though not computed.
        let at = 3 + 8;
        for (offset, byte) in [(at + 4, 1), (at + 5, 1), (at + 6, 1), (at + 8, 1)] {
            let mut bad = stopped.clone();
            bad[offset] = byte;
            assert!(parse_block_value_payee(&bad, Verdict::Accept).is_none(), "{offset}");
        }

        let pool = CreditPool {
            locked: 3,
            current_limit: 2,
            lately_unlocked: 1,
            indexes: UnlockIndexes::from_indexes([4, 5, 9]),
        };
        let mut step = vec![1u8, 0];
        step.extend(0i64.to_le_bytes());
        step.push(1);
        pool.write(&mut step);
        let parsed = parse_credit_pool_step(&step, Verdict::Accept).unwrap();
        assert_eq!((parsed.checked, parsed.expected, parsed.pool), (true, None, Some(pool)));
        assert!(parse_credit_pool_step(&step[..step.len() - 1], Verdict::Accept).is_none());
    }
}
