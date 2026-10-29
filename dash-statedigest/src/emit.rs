//! Records as an emitter builds them (tasks O9a-2, CH3a a7-1): a `C` with the header keys of spec
//! §5.1, the derived keys of §5.2 a writer computes outside the evo hooks ([`with_derived`]) and
//! the golden keys of §5.3 ([`with_golden`]), and the result keys of `R` (§4.3) and `D` (§4.4),
//! from plain values.
//!
//! An emitter that computes only some keys leaves the others absent, never `null`: an absent key
//! is uncovered in a comparison (§10.3), where a `null` would claim a value. A key whose era makes
//! it `null` (genesis's `undo` and `stats`, §5) is written as `null` by the writer that computes
//! it. The same holds for `fx`: an emitter that does not observe effects leaves it absent rather
//! than writing `[]`. The evo keys `mnl`, `qc`, `cp` and `ehf` have no setter here: their writer is
//! the evo hooks' (CH7a's H11).

use crate::{FieldError, Hash256, Ident, Outcome, Value};

/// The header keys of a `C` (§5.1), from `pindex` and the block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderFields {
    /// `pindex->pprev->GetBlockHash()`; `None` for genesis.
    pub prev: Option<Hash256>,
    pub ver: i32,
    pub mrkl: Hash256,
    pub time: u32,
    pub bits: u32,
    pub nonce: u32,
    /// `nChainWork` as a `uint256`, display order.
    pub work: Hash256,
    /// `block.vtx.size()`.
    pub ntx: u64,
}

/// The `R` results (§4.3) an emitter writes; `res:"ok"` belongs to `fJustCheck` attempts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectResult<'a> {
    /// `state.IsInvalid()`: the `BlockValidationResult` name (`consensus`, …) and the reject
    /// reason.
    Invalid {
        cls: &'a str,
        why: &'a str,
    },
    /// `state.IsError()`: the reject reason, `unset` when none was set, or `exception`.
    Error {
        why: &'a str,
    },
    /// A `fJustCheck` attempt that passed.
    Ok,
}

fn seq_only(mut o: Outcome, seq: i64) -> Result<Outcome, FieldError> {
    o.x_mut().set("seq", Value::Int(seq))?;
    Ok(o)
}

/// A `C` holding the header keys and `seq`.
pub fn connect(id: Ident, header: &HeaderFields, seq: i64) -> Result<Outcome, FieldError> {
    let mut c = Outcome::connect(id);
    let mut s = c.s_mut();
    s.set("prev", header.prev.as_ref().map_or(Value::Null, Value::hash))?;
    s.set("ver", Value::Int(header.ver.into()))?;
    s.set("mrkl", Value::hash(&header.mrkl))?;
    s.set("time", Value::Int(header.time.into()))?;
    s.set("bits", Value::Int(header.bits.into()))?;
    s.set("nonce", Value::Int(header.nonce.into()))?;
    s.set("work", Value::hash(&header.work))?;
    // The count of a block's transactions fits an i64: a block is at most 2 MB.
    s.set("ntx", Value::Int(header.ntx as i64))?;
    seq_only(c, seq)
}

/// An `R` holding `res`, `cls`, `why`, `seq` and, if given, `dbg`.
pub fn reject(
    id: Ident,
    result: RejectResult<'_>,
    dbg: Option<&str>,
    seq: i64,
) -> Result<Outcome, FieldError> {
    let mut r = Outcome::reject(id);
    let (res, cls, why) = match result {
        RejectResult::Invalid {
            cls,
            why,
        } => ("invalid", Value::string(cls), Value::string(why)),
        RejectResult::Error {
            why,
        } => ("error", Value::Null, Value::string(why)),
        RejectResult::Ok => ("ok", Value::Null, Value::Null),
    };
    let mut s = r.s_mut();
    s.set("res", Value::string(res))?;
    s.set("cls", cls)?;
    s.set("why", why)?;
    r.x_mut().set("dbg", dbg.map_or(Value::Null, Value::string))?;
    seq_only(r, seq)
}

/// A `D` holding `res` (`ok`, `unclean` or `failed`) and `seq`.
pub fn disconnect(id: Ident, res: &str, seq: i64) -> Result<Outcome, FieldError> {
    let mut d = Outcome::disconnect(id);
    d.s_mut().set("res", Value::string(res))?;
    seq_only(d, seq)
}

/// One `dep` row (§5.2): a versionbits deployment's answers for the child of the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepRow<'a> {
    /// `VersionBitsDeploymentInfo[pos].name`.
    pub name: &'a str,
    /// The `ThresholdState`'s name: `defined`, `started`, `locked_in`, `active` or `failed`.
    pub state: &'a str,
    /// `StateSinceHeight`.
    pub since: i32,
}

/// The derived keys of a `C` (§5.2) a writer computes outside the evo hooks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DerivedFields<'a> {
    /// `sha256d` of the undo data `WriteUndoDataForBlock` receives, display order; `None` is
    /// genesis's `null`.
    pub undo: Option<Hash256>,
    /// `!DeploymentActiveAfter(pindex, V19)`: the child decodes with the legacy BLS scheme.
    pub bls: bool,
    /// Bit *i* set iff the *i*-th `Consensus::BuriedDeployment` is active after the block.
    pub bur: u32,
    /// One row per `DeploymentPos` in enum order; `None` leaves `dep` absent (the writer's view
    /// keeps no versionbits cache).
    pub dep: Option<&'a [DepRow<'a>]>,
}

/// Sets the derived keys `undo`, `bls`, `bur` and, if given, `dep` on the `C` record `c`. On `Err`
/// the keys before the failing one are set: a writer treats it as a failed write.
pub fn with_derived(c: &mut Outcome, derived: &DerivedFields<'_>) -> Result<(), FieldError> {
    let mut s = c.s_mut();
    s.set("undo", derived.undo.as_ref().map_or(Value::Null, Value::hash))?;
    s.set("bls", Value::Bool(derived.bls))?;
    s.set("bur", Value::Int(derived.bur.into()))?;
    if let Some(rows) = derived.dep {
        let rows = rows.iter().map(|r| {
            Value::Arr(vec![
                Value::string(r.name),
                Value::string(r.state),
                Value::Int(r.since.into()),
            ])
        });
        s.set("dep", Value::Arr(rows.collect()))?;
    }
    Ok(())
}

/// The golden keys of a `C` (§5.3): the coinstats index's answers at the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GoldenFields {
    /// `gettxoutsetinfo muhash`'s `muhash`, as the RPC prints it (display order); `None` leaves
    /// `muhash` absent (a writer that finalizes the hash at chosen blocks only, CH3a D165).
    pub muhash: Option<Hash256>,
    pub txouts: u64,
    /// `total_amount`.
    pub amount: i64,
    /// `total_unspendable_amount`.
    pub unspendable: i64,
    /// `block_info`: `prevout_spent`, `coinbase`, `new_outputs_ex_coinbase`, `unspendable`, then
    /// `unspendables`' `genesis_block`, `scripts` and `unclaimed_rewards`.
    pub bi: [i64; 7],
    /// `getblockstats`' `subsidy`, `totalfee`, `txs`, `ins`, `outs`, `utxo_increase` and
    /// `utxo_size_inc`; `None` is genesis's `null`.
    pub stats: Option<[i64; 7]>,
}

fn ints(values: &[i64]) -> Value {
    Value::Arr(values.iter().map(|&v| Value::Int(v)).collect())
}

/// Sets the golden keys on the `C` record `c`, `muhash` only if it is given.
pub fn with_golden(c: &mut Outcome, golden: &GoldenFields) -> Result<(), FieldError> {
    let txouts = i64::try_from(golden.txouts).map_err(|_| FieldError::Shape("txouts above i64"))?;
    let mut s = c.s_mut();
    if let Some(muhash) = &golden.muhash {
        s.set("muhash", Value::hash(muhash))?;
    }
    s.set("txouts", Value::Int(txouts))?;
    s.set("amount", Value::Int(golden.amount))?;
    s.set("unspendable", Value::Int(golden.unspendable))?;
    s.set("bi", ints(&golden.bi))?;
    s.set("stats", golden.stats.as_ref().map_or(Value::Null, |stats| ints(stats)))?;
    Ok(())
}
