//! Governance objects and votes (ABI 26, v24 only; PLAN O2a-GOV, DEC-238).
//!
//! [`Oracle::gov_proposal_validate`] is `governance::ValidateProposal()`;
//! [`Oracle::gov_object`] and [`Oracle::gov_vote`] are one object's and one vote's checks on a
//! chain context; [`Oracle::gov_ops`] runs an op script through Core's `CGovernanceManager` at
//! the context's tip: the single-object and single-vote verdicts a node gives, the tallies and
//! the best superblock. [`Oracle::block_value_payee_scripted`] runs the payment checks over the
//! triggers such a script leaves held (payments context format 2). The formats are in `oracle.h`
//! ("governance") and `docs/node-program/designs/O2a-GOV-notes.md`.

use super::*;
use crate::addrman::{take_hash, take_le, take_list, take_string, take_u8, take_vec};
use crate::payments::{BlockValuePayee, PaymentsContext, TxOut, malformed};

/// `GovernanceObject::PROPOSAL`.
pub const OBJECT_PROPOSAL: i32 = 1;
/// `GovernanceObject::TRIGGER`.
pub const OBJECT_TRIGGER: i32 = 2;

/// A fee transaction the node's transaction index holds (oracle.h "governance context").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collateral {
    /// Exactly one transaction in network serialization (the governance context is decoded
    /// whole, so anything else is refused or misread as the next field).
    pub tx: Vec<u8>,
    /// The block holding it (Core's byte order).
    pub block_hash: [u8; 32],
    /// Whether that block is one of the chain context's headers (the active chain).
    pub active: bool,
}

fn write_collaterals(out: &mut Vec<u8>, collaterals: &[Collateral]) {
    write_compact_size(out, collaterals.len() as u64);
    for c in collaterals {
        out.extend(&c.tx);
        out.extend(c.block_hash);
        out.push(c.active.into());
    }
}

/// The node's governance state besides the chain context (oracle.h "governance context").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GovContext {
    /// `CMasternodeSync::IsSynced()`.
    pub synced: bool,
    pub collaterals: Vec<Collateral>,
}

impl GovContext {
    /// The shim's governance-context bytes (format 1).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = vec![1u8, self.synced.into()];
        write_collaterals(&mut out, &self.collaterals);
        out
    }
}

/// One op of a governance script (oracle.h "governance", `oracle_gov_ops`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GovOp {
    /// `SetMockTime(t)`.
    Time(i64),
    /// An `MNGOVERNANCEOBJECT` payload.
    Object(Vec<u8>),
    /// An `MNGOVERNANCEOBJECTVOTE` payload.
    Vote(Vec<u8>),
    /// A held object's state.
    State([u8; 32]),
    /// The superblock at a height.
    Superblock(i32),
}

impl GovOp {
    fn write(&self, out: &mut Vec<u8>) {
        match self {
            GovOp::Time(t) => {
                out.push(0);
                out.extend(t.to_le_bytes());
            }
            GovOp::Object(bytes) => {
                out.push(1);
                write_bytes(out, bytes);
            }
            GovOp::Vote(bytes) => {
                out.push(2);
                write_bytes(out, bytes);
            }
            GovOp::State(hash) => {
                out.push(3);
                out.extend(hash);
            }
            GovOp::Superblock(height) => {
                out.push(4);
                out.extend(height.to_le_bytes());
            }
        }
    }
}

fn write_ops(out: &mut Vec<u8>, ops: &[GovOp]) {
    for op in ops {
        op.write(out);
    }
}

/// The governance part of a format-2 payments context: the triggers are what `ops` leaves held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaymentsScript {
    pub collaterals: Vec<Collateral>,
    pub ops: Vec<GovOp>,
}

/// One payment of a trigger: its script, amount and proposal hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovPayment {
    pub script: Vec<u8>,
    pub amount: i64,
    pub proposal_hash: [u8; 32],
}

/// `CSuperblock::IsExpired()` of a held trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expiry {
    Live,
    Expired,
    /// The event height plus the status's horizon overflows `int` (undefined behaviour in Core).
    Overflow,
}

/// The superblock `CSuperblock`'s constructor parses from a trigger's data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedTrigger {
    pub height: i32,
    pub payments: Vec<GovPayment>,
    /// `CSuperblock::IsValidBlockHeight()`.
    pub valid_height: bool,
    /// `GetPaymentsLimit()`; `None` where Core's subsidy read at `height - 1` asserts or
    /// overflows.
    pub payments_limit: Option<i64>,
    /// `IsExpired()` at the tip's height with the Valid status (after `AddTrigger()`).
    pub expired_valid: Expiry,
    /// The same with the Executed status (after `ExecuteBestSuperblock()`).
    pub expired_executed: Expiry,
    /// `GetHexStrData()`.
    pub data: String,
}

/// Result of [`Oracle::gov_object`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovObjectCheck {
    pub hash: [u8; 32],
    pub data_hash: [u8; 32],
    pub signature_hash: [u8; 32],
    pub object_type: i32,
    /// `GetMinCollateralFee()`.
    pub min_fee: i64,
    /// `IsValidLocally()` without the collateral (`UpdateLocalValidity()`).
    pub local_valid: bool,
    pub local_error: String,
    /// `IsValidLocally()` with it (`ProcessObject()`).
    pub valid: bool,
    pub error: String,
    pub missing_confirmations: bool,
    /// The trigger parse, or the parser's exception message.
    pub trigger: std::result::Result<ParsedTrigger, String>,
}

/// Result of [`Oracle::gov_vote`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovVoteCheck {
    pub hash: [u8; 32],
    pub signature_hash: [u8; 32],
    pub signature_string: String,
    /// Whether `IsValid()` reaches its list lookup (time, signal and outcome pass).
    pub reads_list: bool,
    /// `GetMNByCollateral()` of the vote's outpoint in the tip's list.
    pub mn_found: bool,
    /// `IsValid()` with the voting key.
    pub valid_voting: bool,
    /// `IsValid()` with the operator key.
    pub valid_operator: bool,
    /// `IsValidForUnknownParent()`.
    pub valid_unknown_parent: bool,
}

/// `governance_exception_type_enum_t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GovExceptionType {
    None = 0,
    Warning = 1,
    PermanentError = 2,
    TemporaryError = 3,
    InternalError = 4,
}

/// `ProcessObject()`'s outcome for an object op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectOutcome {
    pub hash: [u8; 32],
    pub ok: bool,
    /// Whether the manager holds it afterwards.
    pub held: bool,
    /// Postponed for missing collateral confirmations.
    pub postponed: bool,
}

/// `ProcessVote()`'s outcome for a vote op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoteOutcome {
    pub hash: [u8; 32],
    pub ok: bool,
    pub exception: GovExceptionType,
    pub message: String,
    pub penalty: i32,
    /// The parent object to request (an orphan vote), if any.
    pub to_request: Option<[u8; 32]>,
}

/// One signal's tallies over the tip's list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub yes: i32,
    pub no: i32,
    pub abstain: i32,
    /// `GetUniqueVoterCount()`: regular masternodes and evonodes.
    pub unique_regular: u16,
    pub unique_evo: u16,
}

/// A masternode's current vote on one signal (`vote_instance_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoteInstance {
    pub signal: i32,
    pub outcome: i32,
    pub last_update: i64,
    pub creation_time: i64,
}

/// A collateral outpoint: the transaction id (internal order) and the output index.
pub type Outpoint = ([u8; 32], u32);

/// A held object's state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectState {
    pub object_type: i32,
    pub cached_funding: bool,
    pub cached_valid: bool,
    pub cached_delete: bool,
    pub cached_endorsed: bool,
    pub dirty: bool,
    pub expired: bool,
    pub deletion_time: i64,
    /// Signals funding, valid, delete and endorsed.
    pub tallies: [Tally; 4],
    /// `mapCurrentMNVotes`: (collateral txid, vout) and its instances.
    pub current_votes: Vec<(Outpoint, Vec<VoteInstance>)>,
    /// The vote file's count.
    pub vote_count: u32,
}

/// The superblock at a height.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuperblockOutcome {
    /// `IsSuperblockTriggered()`.
    pub triggered: bool,
    /// `GetBestSuperblock()`'s object hash.
    pub best: Option<[u8; 32]>,
    /// `GetSuperblockPayments()` of the best.
    pub payments: Vec<TxOut>,
}

/// One op's output, in op order (`Time` has none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GovOutput {
    /// `None`: the message did not decode.
    Object(Option<ObjectOutcome>),
    /// `None`: the message did not decode (the peer is punished 100).
    Vote(Option<VoteOutcome>),
    /// `None`: no such object is held.
    State(Option<Box<ObjectState>>),
    Superblock(SuperblockOutcome),
}

/// A 0/1 byte.
fn take_flag(r: &mut &[u8]) -> Option<bool> {
    match take_u8(r)? {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// A [`take_flag`] presence byte, then the item when it is 1.
fn take_flagged<T>(r: &mut &[u8], item: impl FnOnce(&mut &[u8]) -> Option<T>) -> Option<Option<T>> {
    Some(if take_flag(r)? {
        Some(item(r)?)
    } else {
        None
    })
}

fn take_expiry(r: &mut &[u8]) -> Option<Expiry> {
    match take_u8(r)? {
        0 => Some(Expiry::Live),
        1 => Some(Expiry::Expired),
        2 => Some(Expiry::Overflow),
        _ => None,
    }
}

fn take_txouts(r: &mut &[u8]) -> Option<Vec<TxOut>> {
    take_list(r, |r| {
        Some(TxOut {
            value: take_le(r)?,
            script_pubkey: take_vec(r)?,
        })
    })
}

fn nonnull(hash: [u8; 32]) -> Option<[u8; 32]> {
    (hash != [0; 32]).then_some(hash)
}

fn parse_trigger(r: &mut &[u8]) -> Option<std::result::Result<ParsedTrigger, String>> {
    let parsed = take_flag(r)?;
    let error = take_string(r)?;
    if !parsed {
        return Some(Err(error));
    }
    if !error.is_empty() {
        return None;
    }
    let height = take_le(r)?;
    let payments = take_list(r, |r| {
        Some(GovPayment {
            script: take_vec(r)?,
            amount: take_le(r)?,
            proposal_hash: take_hash(r)?,
        })
    })?;
    let valid_height = take_flag(r)?;
    let limit: i64 = take_le(r)?;
    // GetPaymentsLimit() is 0 off a superblock height; -1 is the shim's refused-height sentinel.
    let payments_limit = match (valid_height, limit) {
        (false, 0) => Some(0),
        (true, -1) => None,
        (true, l) if l >= 0 => Some(l),
        _ => return None,
    };
    Some(Ok(ParsedTrigger {
        height,
        payments,
        valid_height,
        payments_limit,
        expired_valid: take_expiry(r)?,
        expired_executed: take_expiry(r)?,
        data: take_string(r)?,
    }))
}

fn parse_gov_object(mut r: &[u8]) -> Option<GovObjectCheck> {
    let r = &mut r;
    let record = GovObjectCheck {
        hash: take_hash(r)?,
        data_hash: take_hash(r)?,
        signature_hash: take_hash(r)?,
        object_type: take_le(r)?,
        min_fee: take_le(r)?,
        local_valid: take_flag(r)?,
        local_error: take_string(r)?,
        valid: take_flag(r)?,
        error: take_string(r)?,
        missing_confirmations: take_flag(r)?,
        trigger: parse_trigger(r)?,
    };
    r.is_empty().then_some(record)
}

fn parse_gov_vote(mut r: &[u8]) -> Option<GovVoteCheck> {
    let r = &mut r;
    let record = GovVoteCheck {
        hash: take_hash(r)?,
        signature_hash: take_hash(r)?,
        signature_string: take_string(r)?,
        reads_list: take_flag(r)?,
        mn_found: take_flag(r)?,
        valid_voting: take_flag(r)?,
        valid_operator: take_flag(r)?,
        valid_unknown_parent: take_flag(r)?,
    };
    // Without the list read the list is empty: no masternode, no valid signature.
    let consistent = record.reads_list
        || !(record.mn_found
            || record.valid_voting
            || record.valid_operator
            || record.valid_unknown_parent);
    (r.is_empty() && consistent).then_some(record)
}

fn take_exception(r: &mut &[u8]) -> Option<GovExceptionType> {
    Some(match take_le::<i32>(r)? {
        0 => GovExceptionType::None,
        1 => GovExceptionType::Warning,
        2 => GovExceptionType::PermanentError,
        3 => GovExceptionType::TemporaryError,
        4 => GovExceptionType::InternalError,
        _ => return None,
    })
}

fn take_tally(r: &mut &[u8]) -> Option<Tally> {
    Some(Tally {
        yes: take_le(r)?,
        no: take_le(r)?,
        abstain: take_le(r)?,
        unique_regular: take_le(r)?,
        unique_evo: take_le(r)?,
    })
}

fn take_vote_instance(r: &mut &[u8]) -> Option<VoteInstance> {
    Some(VoteInstance {
        signal: take_le(r)?,
        outcome: take_le(r)?,
        last_update: take_le(r)?,
        creation_time: take_le(r)?,
    })
}

fn take_state(r: &mut &[u8]) -> Option<ObjectState> {
    // The fields are read in the order the shim writes them.
    Some(ObjectState {
        object_type: take_le(r)?,
        cached_funding: take_flag(r)?,
        cached_valid: take_flag(r)?,
        cached_delete: take_flag(r)?,
        cached_endorsed: take_flag(r)?,
        dirty: take_flag(r)?,
        expired: take_flag(r)?,
        deletion_time: take_le(r)?,
        tallies: [take_tally(r)?, take_tally(r)?, take_tally(r)?, take_tally(r)?],
        current_votes: take_list(r, |r| {
            let outpoint = (take_hash(r)?, take_le(r)?);
            Some((outpoint, take_list(r, take_vote_instance)?))
        })?,
        vote_count: take_le(r)?,
    })
}

fn take_object_outcome(r: &mut &[u8]) -> Option<ObjectOutcome> {
    Some(ObjectOutcome {
        hash: take_hash(r)?,
        ok: take_flag(r)?,
        held: take_flag(r)?,
        postponed: take_flag(r)?,
    })
}

fn take_vote_outcome(r: &mut &[u8]) -> Option<VoteOutcome> {
    Some(VoteOutcome {
        hash: take_hash(r)?,
        ok: take_flag(r)?,
        exception: take_exception(r)?,
        message: take_string(r)?,
        penalty: take_le(r)?,
        to_request: nonnull(take_hash(r)?),
    })
}

fn take_superblock(r: &mut &[u8]) -> Option<SuperblockOutcome> {
    let triggered = take_flag(r)?;
    let found = take_flag(r)?;
    let best = take_hash(r)?;
    let payments = take_txouts(r)?;
    // A best superblock is found exactly when its hash is not null, and only then has payments.
    if found == (best == [0; 32]) || (!found && !payments.is_empty()) {
        return None;
    }
    Some(SuperblockOutcome {
        triggered,
        best: found.then_some(best),
        payments,
    })
}

fn parse_gov_ops(mut r: &[u8], ops: &[GovOp]) -> Option<Vec<GovOutput>> {
    let r = &mut r;
    let mut outputs = Vec::new();
    for op in ops {
        outputs.push(match op {
            GovOp::Time(_) => continue,
            GovOp::Object(_) => GovOutput::Object(take_flagged(r, take_object_outcome)?),
            GovOp::Vote(_) => GovOutput::Vote(take_flagged(r, take_vote_outcome)?),
            GovOp::State(_) => GovOutput::State(take_flagged(r, |r| take_state(r).map(Box::new))?),
            GovOp::Superblock(_) => GovOutput::Superblock(take_superblock(r)?),
        });
    }
    r.is_empty().then_some(outputs)
}

impl Oracle {
    /// `governance::ValidateProposal(data_hex, ...)` with `chain`'s parameters and
    /// `GetAdjustedTime()` = `adjusted_time`: the verdict and the error string.
    pub fn gov_proposal_validate(
        &self,
        chain: &Chain,
        data_hex: &str,
        check_expiration: bool,
        allow_script: bool,
        adjusted_time: i64,
    ) -> Result<(bool, String)> {
        self.on_chain(chain, || {
            let mut valid = 0;
            let errors = self.with_buffer(256, |out, len| unsafe {
                (self.api.gov_proposal_validate)(
                    data_hex.as_ptr().cast(),
                    data_hex.len(),
                    check_expiration.into(),
                    allow_script.into(),
                    adjusted_time,
                    &mut valid,
                    out.cast(),
                    len,
                )
            })?;
            // Exactly one NUL, the terminator: Core's messages are fixed strings.
            let text = CStr::from_bytes_with_nul(&errors)
                .ok()
                .filter(|_| valid == 0 || valid == 1)
                .ok_or_else(|| malformed("oracle_gov_proposal_validate"))?;
            Ok((valid != 0, text.to_string_lossy().into_owned()))
        })
    }

    /// One governance object's hashes, local validity, and trigger parse on `ctx`.
    pub fn gov_object(
        &self,
        chain: &Chain,
        object: &[u8],
        ctx: &Context,
        gov: &GovContext,
    ) -> Result<GovObjectCheck> {
        let ctx = ctx.serialize();
        let gov = gov.serialize();
        self.on_chain(chain, || {
            let record = self.with_buffer(1024, |out, len| unsafe {
                (self.api.gov_object)(
                    object.as_ptr(),
                    object.len(),
                    ctx.as_ptr(),
                    ctx.len(),
                    gov.as_ptr(),
                    gov.len(),
                    out,
                    len,
                )
            })?;
            parse_gov_object(&record).ok_or_else(|| malformed("oracle_gov_object"))
        })
    }

    /// One governance vote's hashes and `IsValid()` checks against the tip's list on `ctx`.
    pub fn gov_vote(&self, chain: &Chain, vote: &[u8], ctx: &Context) -> Result<GovVoteCheck> {
        let ctx = ctx.serialize();
        self.on_chain(chain, || {
            let record = self.with_buffer(512, |out, len| unsafe {
                (self.api.gov_vote)(vote.as_ptr(), vote.len(), ctx.as_ptr(), ctx.len(), out, len)
            })?;
            parse_gov_vote(&record).ok_or_else(|| malformed("oracle_gov_vote"))
        })
    }

    /// Run `ops` through a `CGovernanceManager` at the tip of `ctx`.
    pub fn gov_ops(
        &self,
        chain: &Chain,
        ctx: &Context,
        gov: &GovContext,
        ops: &[GovOp],
    ) -> Result<Vec<GovOutput>> {
        let ctx = ctx.serialize();
        let gov = gov.serialize();
        let mut script = Vec::new();
        write_ops(&mut script, ops);
        self.on_chain(chain, || {
            let record = self.with_buffer(4096, |out, len| unsafe {
                (self.api.gov_ops)(
                    ctx.as_ptr(),
                    ctx.len(),
                    gov.as_ptr(),
                    gov.len(),
                    script.as_ptr(),
                    script.len(),
                    out,
                    len,
                )
            })?;
            parse_gov_ops(&record, ops).ok_or_else(|| malformed("oracle_gov_ops"))
        })
    }

    /// [`Oracle::block_value_payee`] with a format-2 payments context: the triggers are the
    /// ones `script` leaves held. `payments.triggers` must be empty.
    pub fn block_value_payee_scripted(
        &self,
        chain: &Chain,
        block: &[u8],
        ctx: &Context,
        payments: &PaymentsContext,
        script: &PaymentsScript,
    ) -> Result<BlockValuePayee> {
        if !payments.triggers.is_empty() {
            return Err(Error {
                kind: ErrorKind::Argument,
                message: "a scripted payments context takes its triggers from the script".into(),
            });
        }
        let mut pay = vec![2u8];
        pay.extend(payments.fee_reward.to_le_bytes());
        pay.push(payments.synced.into());
        pay.extend(payments.best_chainlock_height.unwrap_or(-1).to_le_bytes());
        pay.push(payments.governance_loaded.into());
        write_collaterals(&mut pay, &script.collaterals);
        let mut ops = Vec::new();
        write_ops(&mut ops, &script.ops);
        write_bytes(&mut pay, &ops);
        self.block_value_payee_raw(chain, block, ctx, &pay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_parse_exactly() {
        let mut vote = Vec::new();
        vote.extend([1u8; 32]);
        vote.extend([2u8; 32]);
        vote.extend([1, b's']);
        vote.extend([1, 1, 0, 1, 1]);
        let parsed = parse_gov_vote(&vote).unwrap();
        assert!(parsed.mn_found && !parsed.valid_voting && parsed.valid_operator);
        assert!(parse_gov_vote(&vote[..vote.len() - 1]).is_none());
        // Without the list read nothing can be found.
        let mut unread = vote.clone();
        let at = unread.len() - 5;
        unread[at] = 0;
        assert!(parse_gov_vote(&unread).is_none());

        let ops =
            [GovOp::Time(5), GovOp::Vote(vec![]), GovOp::Object(vec![]), GovOp::Superblock(10)];
        let mut record = vec![1u8];
        record.extend([3u8; 32]);
        record.extend([0, 2, 0, 0, 0, 1, b'm', 20, 0, 0, 0]);
        record.extend([4u8; 32]);
        record.push(0);
        record.extend([0, 0]);
        record.extend([0u8; 32]);
        record.push(0);
        let outputs = parse_gov_ops(&record, &ops).unwrap();
        let GovOutput::Vote(Some(v)) = &outputs[0] else {
            panic!("{outputs:?}")
        };
        assert_eq!(
            (v.exception, v.penalty, v.to_request),
            (GovExceptionType::PermanentError, 20, Some([4; 32]))
        );
        assert_eq!(outputs[1], GovOutput::Object(None));
        assert_eq!(
            outputs[2],
            GovOutput::Superblock(SuperblockOutcome {
                triggered: false,
                best: None,
                payments: vec![]
            })
        );
        // A best superblock that is "found" with a null hash is not written.
        let mut bad = record.clone();
        let at = bad.len() - 34;
        bad[at] = 1;
        assert!(parse_gov_ops(&bad, &ops).is_none());
        assert!(parse_gov_ops(&[record.as_slice(), &[0]].concat(), &ops).is_none());
    }
}
