//! The typed forms of the `fx` entries (spec §6) and the `q` entries (spec §7.1).
//!
//! Both convert losslessly to and from [`Value`]: `Effect::try_from(&e.to_value()) == Ok(e)` and
//! the same for [`Query`]. The `TryFrom` direction is strict: it accepts exactly the shapes the
//! spec lists.

use core::fmt;

use crate::hex::{Hash256, decode_hex};
use crate::value::Value;

/// A value does not have the shape its position requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapeError(pub &'static str);

impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl core::error::Error for ShapeError {}

type Res<T> = Result<T, ShapeError>;

fn int(v: &Value) -> Res<i64> {
    v.as_int().ok_or(ShapeError("expected an integer"))
}

fn boolean(v: &Value) -> Res<bool> {
    v.as_bool().ok_or(ShapeError("expected a boolean"))
}

fn hash(v: &Value) -> Res<Hash256> {
    v.as_hash().ok_or(ShapeError("expected a hash (64 lowercase hex digits)"))
}

fn hex(v: &Value) -> Res<Vec<u8>> {
    v.as_bytes().and_then(decode_hex).ok_or(ShapeError("expected lowercase hex of even length"))
}

fn head(v: &Value) -> Res<(&[u8], &[Value])> {
    let items = v.as_arr().ok_or(ShapeError("expected an array"))?;
    let (name, rest) = items.split_first().ok_or(ShapeError("empty entry"))?;
    Ok((name.as_bytes().ok_or(ShapeError("entry name must be a string"))?, rest))
}

fn arity(rest: &[Value], n: usize) -> Res<()> {
    if rest.len() == n {
        Ok(())
    } else {
        Err(ShapeError("wrong number of elements"))
    }
}

fn entry(name: &str, rest: Vec<Value>) -> Value {
    let mut items = Vec::with_capacity(rest.len() + 1);
    items.push(Value::string(name));
    items.extend(rest);
    Value::Arr(items)
}

/// One non-transactional action an attempt emitted (spec §6). Hashes are in display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// `["snap",type,cycleBaseHash,sha256d(serialize(CQuorumSnapshot))]`.
    Snap {
        quorum_type: i64,
        cycle_base: Hash256,
        snapshot: Hash256,
    },
    /// `["is_rm",islockHash,txid]`: an InstantSend lock removed by the IS loop.
    IsRm {
        islock: Hash256,
        txid: Hash256,
    },
    /// `["qc_unmine",type,quorumHash]`: a mineable commitment erased.
    QcUnmine {
        quorum_type: i64,
        quorum_hash: Hash256,
    },
    /// `["qc_remine",type,quorumHash]`: a mineable commitment re-inserted by an undo.
    QcRemine {
        quorum_type: i64,
        quorum_hash: Hash256,
    },
    /// `["ban_reset",proTxHash,height,ok]`: `ResetPlatformBan` and its return value.
    BanReset {
        pro_tx_hash: Hash256,
        height: i64,
        ok: bool,
    },
    /// `["mn_changed",undo]`: `NotifyMasternodeListChanged(undo, ...)`.
    MnChanged {
        undo: bool,
    },
}

impl Effect {
    /// The entry as a [`Value`] array.
    pub fn to_value(&self) -> Value {
        match self {
            Effect::Snap {
                quorum_type,
                cycle_base,
                snapshot,
            } => entry(
                "snap",
                vec![Value::Int(*quorum_type), Value::hash(cycle_base), Value::hash(snapshot)],
            ),
            Effect::IsRm {
                islock,
                txid,
            } => entry("is_rm", vec![Value::hash(islock), Value::hash(txid)]),
            Effect::QcUnmine {
                quorum_type,
                quorum_hash,
            } => entry("qc_unmine", vec![Value::Int(*quorum_type), Value::hash(quorum_hash)]),
            Effect::QcRemine {
                quorum_type,
                quorum_hash,
            } => entry("qc_remine", vec![Value::Int(*quorum_type), Value::hash(quorum_hash)]),
            Effect::BanReset {
                pro_tx_hash,
                height,
                ok,
            } => entry(
                "ban_reset",
                vec![Value::hash(pro_tx_hash), Value::Int(*height), Value::Bool(*ok)],
            ),
            Effect::MnChanged {
                undo,
            } => entry("mn_changed", vec![Value::Bool(*undo)]),
        }
    }

    /// The `fx` array for `effects`, in order.
    pub fn list_to_value(effects: &[Effect]) -> Value {
        Value::Arr(effects.iter().map(Effect::to_value).collect())
    }

    /// Reads a whole `fx` array.
    pub fn list_from_value(value: &Value) -> Res<Vec<Effect>> {
        value
            .as_arr()
            .ok_or(ShapeError("fx must be an array"))?
            .iter()
            .map(Effect::try_from)
            .collect()
    }
}

impl TryFrom<&Value> for Effect {
    type Error = ShapeError;

    fn try_from(value: &Value) -> Res<Effect> {
        let (name, rest) = head(value)?;
        match name {
            b"snap" => {
                arity(rest, 3)?;
                Ok(Effect::Snap {
                    quorum_type: int(&rest[0])?,
                    cycle_base: hash(&rest[1])?,
                    snapshot: hash(&rest[2])?,
                })
            }
            b"is_rm" => {
                arity(rest, 2)?;
                Ok(Effect::IsRm {
                    islock: hash(&rest[0])?,
                    txid: hash(&rest[1])?,
                })
            }
            b"qc_unmine" => {
                arity(rest, 2)?;
                Ok(Effect::QcUnmine {
                    quorum_type: int(&rest[0])?,
                    quorum_hash: hash(&rest[1])?,
                })
            }
            b"qc_remine" => {
                arity(rest, 2)?;
                Ok(Effect::QcRemine {
                    quorum_type: int(&rest[0])?,
                    quorum_hash: hash(&rest[1])?,
                })
            }
            b"ban_reset" => {
                arity(rest, 3)?;
                Ok(Effect::BanReset {
                    pro_tx_hash: hash(&rest[0])?,
                    height: int(&rest[1])?,
                    ok: boolean(&rest[2])?,
                })
            }
            b"mn_changed" => {
                arity(rest, 1)?;
                Ok(Effect::MnChanged {
                    undo: boolean(&rest[0])?,
                })
            }
            _ => Err(ShapeError("unknown effect")),
        }
    }
}

/// The best ChainLock a `cl_best` query returned (non-null form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BestChainLock {
    pub height: i64,
    pub hash: Hash256,
    /// The signature bytes as Core serializes them: 96 (spec §7.1).
    pub sig: [u8; 96],
}

/// The conflicting InstantSend lock an `is_conflict` query returned (non-null form).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IsConflictPair {
    pub islock: Hash256,
    pub conflicting_txid: Hash256,
}

/// One payment of a superblock (`sb_payments`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SbPayment {
    /// Duffs.
    pub amount: i64,
    pub script: Vec<u8>,
}

/// One chain-helper query an attempt made and the answer Core used (spec §7.1). Hashes are in
/// display order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    /// `["cl_conflict",h,hash,bool]`.
    ClConflict {
        height: i64,
        hash: Hash256,
        answer: bool,
    },
    /// `["cl_best",null]` (`None`) or `["cl_best",h,hash,sig]`.
    ClBest(Option<BestChainLock>),
    /// `["ibd",bool]`.
    Ibd(bool),
    /// `["cl_has",h,hash,bool]`.
    ClHas {
        height: i64,
        hash: Hash256,
        answer: bool,
    },
    /// `["is_conflict",txid,null]` (`None`) or `["is_conflict",txid,[islockHash,conflictTxid]]`.
    IsConflict {
        txid: Hash256,
        conflict: Option<IsConflictPair>,
    },
    /// `["sb_required",h,bool]`.
    SbRequired {
        height: i64,
        answer: bool,
    },
    /// `["gov_valid",bool]`.
    GovValid(bool),
    /// `["sb_triggered",h,bool]`.
    SbTriggered {
        height: i64,
        answer: bool,
    },
    /// `["sb_payments",h,null]` (`None`: no best superblock) or `["sb_payments",h,[[amount,scriptHex],…]]`
    /// (`Some`, possibly empty).
    SbPayments {
        height: i64,
        payments: Option<Vec<SbPayment>>,
    },
}

impl Query {
    /// The entry as a [`Value`] array.
    pub fn to_value(&self) -> Value {
        match self {
            Query::ClConflict {
                height,
                hash,
                answer,
            } => entry(
                "cl_conflict",
                vec![Value::Int(*height), Value::hash(hash), Value::Bool(*answer)],
            ),
            Query::ClBest(None) => entry("cl_best", vec![Value::Null]),
            Query::ClBest(Some(b)) => entry(
                "cl_best",
                vec![Value::Int(b.height), Value::hash(&b.hash), Value::hex(&b.sig)],
            ),
            Query::Ibd(b) => entry("ibd", vec![Value::Bool(*b)]),
            Query::ClHas {
                height,
                hash,
                answer,
            } => {
                entry("cl_has", vec![Value::Int(*height), Value::hash(hash), Value::Bool(*answer)])
            }
            Query::IsConflict {
                txid,
                conflict,
            } => entry(
                "is_conflict",
                vec![
                    Value::hash(txid),
                    match conflict {
                        None => Value::Null,
                        Some(c) => Value::Arr(vec![
                            Value::hash(&c.islock),
                            Value::hash(&c.conflicting_txid),
                        ]),
                    },
                ],
            ),
            Query::SbRequired {
                height,
                answer,
            } => entry("sb_required", vec![Value::Int(*height), Value::Bool(*answer)]),
            Query::GovValid(b) => entry("gov_valid", vec![Value::Bool(*b)]),
            Query::SbTriggered {
                height,
                answer,
            } => entry("sb_triggered", vec![Value::Int(*height), Value::Bool(*answer)]),
            Query::SbPayments {
                height,
                payments,
            } => entry(
                "sb_payments",
                vec![
                    Value::Int(*height),
                    match payments {
                        None => Value::Null,
                        Some(list) => Value::Arr(
                            list.iter()
                                .map(|p| {
                                    Value::Arr(vec![Value::Int(p.amount), Value::hex(&p.script)])
                                })
                                .collect(),
                        ),
                    },
                ],
            ),
        }
    }

    /// The `q` array for `queries`, in call order.
    pub fn list_to_value(queries: &[Query]) -> Value {
        Value::Arr(queries.iter().map(Query::to_value).collect())
    }

    /// Reads a whole `q` array.
    pub fn list_from_value(value: &Value) -> Res<Vec<Query>> {
        value
            .as_arr()
            .ok_or(ShapeError("q must be an array"))?
            .iter()
            .map(Query::try_from)
            .collect()
    }
}

impl TryFrom<&Value> for Query {
    type Error = ShapeError;

    fn try_from(value: &Value) -> Res<Query> {
        let (name, rest) = head(value)?;
        match name {
            b"cl_conflict" => {
                arity(rest, 3)?;
                Ok(Query::ClConflict {
                    height: int(&rest[0])?,
                    hash: hash(&rest[1])?,
                    answer: boolean(&rest[2])?,
                })
            }
            b"cl_best" => match rest {
                [Value::Null] => Ok(Query::ClBest(None)),
                [h, b, sig] => Ok(Query::ClBest(Some(BestChainLock {
                    height: int(h)?,
                    hash: hash(b)?,
                    sig: <[u8; 96]>::try_from(hex(sig)?)
                        .map_err(|_| ShapeError("a signature is 96 bytes of hex"))?,
                }))),
                _ => Err(ShapeError("cl_best takes null or height, hash, sig")),
            },
            b"ibd" => {
                arity(rest, 1)?;
                Ok(Query::Ibd(boolean(&rest[0])?))
            }
            b"cl_has" => {
                arity(rest, 3)?;
                Ok(Query::ClHas {
                    height: int(&rest[0])?,
                    hash: hash(&rest[1])?,
                    answer: boolean(&rest[2])?,
                })
            }
            b"is_conflict" => {
                arity(rest, 2)?;
                let conflict = match &rest[1] {
                    Value::Null => None,
                    pair => match pair.as_arr() {
                        Some([a, b]) => Some(IsConflictPair {
                            islock: hash(a)?,
                            conflicting_txid: hash(b)?,
                        }),
                        _ => return Err(ShapeError("is_conflict answer is null or a pair")),
                    },
                };
                Ok(Query::IsConflict {
                    txid: hash(&rest[0])?,
                    conflict,
                })
            }
            b"sb_required" => {
                arity(rest, 2)?;
                Ok(Query::SbRequired {
                    height: int(&rest[0])?,
                    answer: boolean(&rest[1])?,
                })
            }
            b"gov_valid" => {
                arity(rest, 1)?;
                Ok(Query::GovValid(boolean(&rest[0])?))
            }
            b"sb_triggered" => {
                arity(rest, 2)?;
                Ok(Query::SbTriggered {
                    height: int(&rest[0])?,
                    answer: boolean(&rest[1])?,
                })
            }
            b"sb_payments" => {
                arity(rest, 2)?;
                let payments = match &rest[1] {
                    Value::Null => None,
                    list => {
                        let items = list
                            .as_arr()
                            .ok_or(ShapeError("sb_payments answer is null or a list"))?;
                        let mut out = Vec::with_capacity(items.len());
                        for item in items {
                            match item.as_arr() {
                                Some([amount, script]) => out.push(SbPayment {
                                    amount: int(amount)?,
                                    script: hex(script)?,
                                }),
                                _ => return Err(ShapeError("a payment is [amount, scriptHex]")),
                            }
                        }
                        Some(out)
                    }
                };
                Ok(Query::SbPayments {
                    height: int(&rest[0])?,
                    payments,
                })
            }
            _ => Err(ShapeError("unknown query")),
        }
    }
}
