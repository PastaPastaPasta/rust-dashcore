//! Core's coins views driven by an op script (ABI 15; `src/test/oracle/oracle.h`, "coins
//! cache"): `CCoinsViewDB` on an in-memory LevelDB, a `CCoinsViewCache` tip over it and child
//! views over the tip, made fresh for each call.
//!
//! Coins travel as the coins database stores them (Core's `Coin` serialization: VARINT code,
//! compressed `CTxOut`), outpoints as a txid in Core's byte order and an output index.

use super::*;
use crate::addrman::{
    take_bool, take_hash, take_le, take_list, take_optional, take_string, take_u8, take_vec,
};

/// `CCoinsCacheEntry::DIRTY`.
pub const DIRTY: u8 = 0x01;
/// `CCoinsCacheEntry::FRESH`.
pub const FRESH: u8 = 0x02;

/// Core's `nDefaultDbBatchSize` (16 MiB): `-dbbatchsize`'s default.
pub const DEFAULT_DB_BATCH_SIZE: i64 = 16 << 20;

/// The tip, the view every script starts with.
pub const TIP: u8 = 0;

/// A `COutPoint`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutPoint {
    pub txid: [u8; 32],
    pub vout: u32,
}

impl OutPoint {
    fn encode(&self, out: &mut Vec<u8>) {
        out.extend(self.txid);
        out.extend(self.vout.to_le_bytes());
    }
}

/// A cache entry: its flags (`DIRTY`, `FRESH`) and its coin, `None` when spent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheEntry {
    pub flags: u8,
    pub coin: Option<Vec<u8>>,
}

/// One op of an `oracle_coins_ops` script; `view` is 0 for the tip, i for the i-th child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoinsOp {
    /// A child `CCoinsViewCache` over the top view.
    Push,
    /// Drop the top child unflushed (a failed connect).
    Pop,
    Add {
        view: u8,
        outpoint: OutPoint,
        coin: Vec<u8>,
        possible_overwrite: bool,
    },
    Spend {
        view: u8,
        outpoint: OutPoint,
        moveout: bool,
    },
    Access {
        view: u8,
        outpoint: OutPoint,
    },
    Have {
        view: u8,
        outpoint: OutPoint,
    },
    HaveInCache {
        view: u8,
        outpoint: OutPoint,
    },
    Flush {
        view: u8,
    },
    Sync {
        view: u8,
    },
    Uncache {
        view: u8,
        outpoint: OutPoint,
    },
    SetBestBlock {
        view: u8,
        hash: [u8; 32],
    },
    GetBestBlock {
        view: u8,
    },
    /// `BatchWrite(map, hash, erase)` of a map built from `entries`.
    BatchWrite {
        view: u8,
        entries: Vec<(OutPoint, CacheEntry)>,
        hash: [u8; 32],
        erase: bool,
    },
    /// `DynamicMemoryUsage()` and `GetCacheSize()`.
    Usage {
        view: u8,
    },
    /// The view's own map entry, read without fetching.
    Entry {
        view: u8,
        outpoint: OutPoint,
    },
    /// Insert an entry into the view's map, as `coins_tests`' `InsertCoinsMapEntry`.
    Seed {
        view: u8,
        outpoint: OutPoint,
        entry: CacheEntry,
    },
    /// The database's state.
    DbState,
}

impl CoinsOp {
    /// The op in the script format.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let head = |out: &mut Vec<u8>, code: u8, view: u8| out.extend([code, view]);
        match self {
            Self::Push => out.push(0),
            Self::Pop => out.push(1),
            Self::Add {
                view,
                outpoint,
                coin,
                possible_overwrite,
            } => {
                head(out, 2, *view);
                outpoint.encode(out);
                write_bytes(out, coin);
                out.push((*possible_overwrite).into());
            }
            Self::Spend {
                view,
                outpoint,
                moveout,
            } => {
                head(out, 3, *view);
                outpoint.encode(out);
                out.push((*moveout).into());
            }
            Self::Access {
                view,
                outpoint,
            } => {
                head(out, 4, *view);
                outpoint.encode(out);
            }
            Self::Have {
                view,
                outpoint,
            } => {
                head(out, 5, *view);
                outpoint.encode(out);
            }
            Self::HaveInCache {
                view,
                outpoint,
            } => {
                head(out, 6, *view);
                outpoint.encode(out);
            }
            Self::Flush {
                view,
            } => head(out, 7, *view),
            Self::Sync {
                view,
            } => head(out, 8, *view),
            Self::Uncache {
                view,
                outpoint,
            } => {
                head(out, 9, *view);
                outpoint.encode(out);
            }
            Self::SetBestBlock {
                view,
                hash,
            } => {
                head(out, 10, *view);
                out.extend(hash);
            }
            Self::GetBestBlock {
                view,
            } => head(out, 11, *view),
            Self::BatchWrite {
                view,
                entries,
                hash,
                erase,
            } => {
                head(out, 12, *view);
                write_compact_size(out, entries.len() as u64);
                for (outpoint, entry) in entries {
                    outpoint.encode(out);
                    entry.encode(out);
                }
                out.extend(hash);
                out.push((*erase).into());
            }
            Self::Usage {
                view,
            } => head(out, 13, *view),
            Self::Entry {
                view,
                outpoint,
            } => {
                head(out, 14, *view);
                outpoint.encode(out);
            }
            Self::Seed {
                view,
                outpoint,
                entry,
            } => {
                head(out, 15, *view);
                outpoint.encode(out);
                entry.encode(out);
            }
            Self::DbState => out.push(16),
        }
    }
}

impl CacheEntry {
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(self.flags);
        match &self.coin {
            None => out.push(0),
            Some(coin) => {
                out.push(1);
                write_bytes(out, coin);
            }
        }
    }
}

/// The coins database after a tip flush or sync, or on request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DbState {
    /// `GetBestBlock()`: all zero when the database has none.
    pub best_block: [u8; 32],
    /// `GetHeadBlocks()`.
    pub head_blocks: Vec<[u8; 32]>,
    /// Every record but the obfuscation key, in key order: raw key, de-obfuscated raw value.
    pub records: Vec<(Vec<u8>, Vec<u8>)>,
}

impl DbState {
    fn decode(r: &mut &[u8]) -> Option<Self> {
        Some(Self {
            best_block: take_hash(r)?,
            head_blocks: take_list(r, take_hash)?,
            records: take_list(r, |r| Some((take_vec(r)?, take_vec(r)?)))?,
        })
    }

    /// The coin records (`'C'` keys) as outpoints and coins, in key order.
    pub fn coins(&self) -> Vec<(OutPoint, Vec<u8>)> {
        self.records
            .iter()
            .filter_map(|(key, value)| {
                let mut r = key.strip_prefix(b"C")?;
                let txid = take_hash(&mut r)?;
                let vout = read_varint(&mut r).filter(|_| r.is_empty())?;
                Some((
                    OutPoint {
                        txid,
                        vout: u32::try_from(vout).ok()?,
                    },
                    value.clone(),
                ))
            })
            .collect()
    }
}

/// Core's `VARINT` (`VarIntMode::DEFAULT`), as the coin keys hold the output index.
fn read_varint(r: &mut &[u8]) -> Option<u64> {
    let mut n = 0u64;
    loop {
        let b = take_u8(r)?;
        n = n.checked_mul(128)? | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some(n);
        }
        n = n.checked_add(1)?;
    }
}

/// What an op returned, by op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoinsOutput {
    /// Push, Pop, Add, Uncache, SetBestBlock, Seed.
    None,
    /// Have, HaveInCache, BatchWrite.
    Bool(bool),
    /// Spend: whether there was an entry to spend, and with moveout the coin moved out
    /// (`None` when the entry was a spent coin).
    Spend {
        spent: bool,
        moved: Option<Option<Vec<u8>>>,
    },
    /// Access: the coin, `None` for Core's empty or spent coin.
    Coin(Option<Vec<u8>>),
    /// Flush, Sync: Core's result, and the database after it when the view is the tip.
    Flushed {
        ok: bool,
        db: Option<DbState>,
    },
    /// GetBestBlock.
    Hash([u8; 32]),
    /// Usage.
    Usage {
        dynamic_usage: u64,
        cache_size: u32,
    },
    /// Entry: the view's own entry.
    Entry(Option<CacheEntry>),
    /// DbState.
    Db(DbState),
}

/// How an op ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoinsOutcome {
    Ok(CoinsOutput),
    /// Core threw `std::logic_error`: its `what()`. The script ends here.
    LogicError(String),
    /// Core would assert: the assertion and where. Not run; the script ends here.
    Assert(String),
}

impl CoinsOutcome {
    fn decode(op: &CoinsOp, r: &mut &[u8]) -> Option<Self> {
        Some(match take_u8(r)? {
            0 => Self::Ok(CoinsOutput::decode(op, r)?),
            1 => Self::LogicError(take_string(r)?),
            2 => Self::Assert(take_string(r)?),
            _ => return None,
        })
    }

    /// Whether the op ended the script.
    pub fn ends_script(&self) -> bool {
        !matches!(self, Self::Ok(_))
    }
}

fn take_opt_coin(r: &mut &[u8]) -> Option<Option<Vec<u8>>> {
    take_optional(r, take_vec)
}

impl CoinsOutput {
    fn decode(op: &CoinsOp, r: &mut &[u8]) -> Option<Self> {
        Some(match op {
            CoinsOp::Push
            | CoinsOp::Pop
            | CoinsOp::Add {
                ..
            }
            | CoinsOp::Uncache {
                ..
            }
            | CoinsOp::SetBestBlock {
                ..
            }
            | CoinsOp::Seed {
                ..
            } => Self::None,
            CoinsOp::Have {
                ..
            }
            | CoinsOp::HaveInCache {
                ..
            }
            | CoinsOp::BatchWrite {
                ..
            } => Self::Bool(take_bool(r)?),
            CoinsOp::Spend {
                moveout,
                ..
            } => {
                let spent = take_bool(r)?;
                let moved = if *moveout && spent {
                    Some(take_opt_coin(r)?)
                } else {
                    None
                };
                Self::Spend {
                    spent,
                    moved,
                }
            }
            CoinsOp::Access {
                ..
            } => Self::Coin(take_opt_coin(r)?),
            CoinsOp::Flush {
                view,
            }
            | CoinsOp::Sync {
                view,
            } => Self::Flushed {
                ok: take_bool(r)?,
                db: if *view == TIP {
                    Some(DbState::decode(r)?)
                } else {
                    None
                },
            },
            CoinsOp::GetBestBlock {
                ..
            } => Self::Hash(take_hash(r)?),
            CoinsOp::Usage {
                ..
            } => Self::Usage {
                dynamic_usage: take_le(r)?,
                cache_size: take_le(r)?,
            },
            CoinsOp::Entry {
                ..
            } => Self::Entry(take_optional(r, |r| {
                Some(CacheEntry {
                    flags: take_u8(r)?,
                    coin: take_opt_coin(r)?,
                })
            })?),
            CoinsOp::DbState => Self::Db(DbState::decode(r)?),
        })
    }
}

/// A script's encoding.
pub fn encode_script(ops: &[CoinsOp]) -> Vec<u8> {
    let mut out = Vec::new();
    for op in ops {
        op.encode(&mut out);
    }
    out
}

impl Oracle {
    /// `oracle_coins_ops`'s raw output for an encoded script.
    pub fn coins_ops_raw(&self, script: &[u8], db_batch_size: i64) -> Result<Vec<u8>> {
        // The shim forces -dbbatchsize in gArgs for the call, which chain calls read and replace.
        self.exclusive(|| {
            self.with_buffer(script.len() * 4 + 1024, |out, len| unsafe {
                (self.api.coins_ops)(script.as_ptr(), script.len(), db_batch_size, out, len)
            })
        })
    }

    /// Runs `ops` with `-dbbatchsize=db_batch_size`: one outcome per op run, in order. The
    /// outcomes stop after the first that ends the script.
    ///
    /// # Errors
    ///
    /// The call's error; [`ErrorKind::Exception`] for output that is not one outcome per op up to
    /// the end of the script.
    pub fn coins_ops(&self, ops: &[CoinsOp], db_batch_size: i64) -> Result<Vec<CoinsOutcome>> {
        let raw = self.coins_ops_raw(&encode_script(ops), db_batch_size)?;
        let mut r = raw.as_slice();
        let mut outcomes = Vec::new();
        for op in ops {
            let outcome =
                CoinsOutcome::decode(op, &mut r).ok_or_else(|| malformed(outcomes.len()))?;
            let ends = outcome.ends_script();
            outcomes.push(outcome);
            if ends {
                break;
            }
        }
        if !r.is_empty() {
            return Err(malformed(outcomes.len()));
        }
        Ok(outcomes)
    }
}

fn malformed(op: usize) -> Error {
    Error {
        kind: ErrorKind::Exception,
        message: format!("oracle_coins_ops output malformed at op {op}"),
    }
}
