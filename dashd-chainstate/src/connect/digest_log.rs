//! [`DigestLog`]: rdashd's StateDigest log (spec O8-statedigest-v1) as [`CoreConnector`]'s
//! [`DigestSink`] (CH3 §2.15; CH3a a7-1). It supersedes O9a-2's header-only `DigestTap` for that
//! connector: each attempt writes one `C`, `R` or `D` (§4.2–§4.4), numbered by one `seq` (§8.1),
//! after an `H` written when the log opens (§4.1).
//!
//! - A `C` is a `ConnectBlock` that connected with `fJustCheck == false`. It holds the header
//!   keys (§5.1, as `DigestTap` builds them), the derived keys `undo`, `bls`, `bur` and `dep`
//!   (§5.2) and, for the active coins, the golden keys (§5.3) from a [`CoinStatsAccumulator`] kept
//!   from genesis (CH3 D34, CH3a D141), or from a seed at a pruned start's base
//!   ([`DigestLog::seeded`], CH3a D165). [`MuHashAt`] picks the blocks whose `muhash` is
//!   finalized and written; at the others the key is absent. The evo keys `mnl`, `qc`, `cp` and
//!   `ehf` are absent: CH7a's H11 writes them.
//! - An `R` is any other `ConnectBlock` outcome, a proposal's pass among them (§4.3).
//! - A `D` holds `DisconnectBlock`'s result (§4.4). A clean disconnect of the active coins also
//!   rewinds the accumulator, as Core's coinstats index rewinds on a reorg
//!   (`index/coinstatsindex.cpp:264-299`, `ReverseBlock` at `:403-505`).
//! - Every record's `org` is [`ConnectCx::origin`] (CH3 D17); `fx` is absent, and no `A` is written
//!   (CH3a D135, D136).
//! - A [`RewindObserver`], if one is set, sees the accumulator's `txouts` and `amount`, and its
//!   `muhash` where [`MuHashAt`] picks the parent, after each clean rewind, at the disconnected
//!   block's parent (CH3a D152): L2's reverse leg compares them with the parent's golden. It
//!   changes no record; an error it returns fails the record as a failed write does: an
//!   `AbortNode`, while the attempt keeps its outcome (CH3a D137).
//!
//! A failed write is an `AbortNode` (§8.3): a log with gaps is worse than none. So is a golden
//! key from an accumulator that missed a block: a `C` whose parent is not the accumulator's block,
//! or a rewind of a block it is not at, is a `Fatal` (CH3a D141). The connector reports either and
//! keeps the attempt's outcome, and the log writes nothing after it, as O8's patch's `Fail` does
//! (CH3a D137).
//!
//! [`CoreConnector`]: super::CoreConnector

use std::collections::BTreeSet;
use std::io::Write;

use dash_consensus::coins::stats::{BlockStats, CoinStatsAccumulator, block_stats};
use dash_consensus::coins::{BlockUndo, BlockUndoRef};
use dash_consensus::ctx::{Deployment, DeploymentPos};
use dash_consensus::{Block, BlockId};
use dash_params::BuriedDeployment;
use dash_statedigest::emit::{
    self, DepRow, DerivedFields, GoldenFields, RejectResult, with_derived, with_golden,
};
use dash_statedigest::{Hash256, Header, Ident, Outcome, Record};

use super::digest::{ConnectAttempt, DigestAttempt, DigestSink, Rewind};
use super::evo::BlockAt;
use crate::chain::digest_tap::{
    class_name, disconnect_res, display, entry, header_fields, origin, write_line,
};
use crate::chain::{ConnectCx, ConnectOutcome, Fatal};

/// `Consensus::BuriedDeployment` in Core's enum order, `bur`'s bit order (O8 §5.2).
// Core: consensus/params.h:22-39
const BURIED: [BuriedDeployment; 15] = [
    BuriedDeployment::HeightInCoinbase,
    BuriedDeployment::DerSig,
    BuriedDeployment::Cltv,
    BuriedDeployment::Bip147,
    BuriedDeployment::Csv,
    BuriedDeployment::Dip0001,
    BuriedDeployment::Dip0003,
    BuriedDeployment::Dip0008,
    BuriedDeployment::Dip0020,
    BuriedDeployment::Dip0024,
    BuriedDeployment::Brr,
    BuriedDeployment::V19,
    BuriedDeployment::V20,
    BuriedDeployment::MnRr,
    BuriedDeployment::Withdrawals,
];

/// `Consensus::DeploymentPos` in Core's enum order with `VersionBitsDeploymentInfo`'s names,
/// `dep`'s row order (O8 §5.2).
// Core: consensus/params.h:42-48; deploymentinfo.cpp:9-22
const VERSIONBITS: [(DeploymentPos, &str); 3] = [
    (DeploymentPos::TestDummy, "testdummy"),
    (DeploymentPos::V24, "v24"),
    (DeploymentPos::EvoShares, "evo_shares"),
];

/// The StateDigest log of a [`CoreConnector`](super::CoreConnector)'s attempts, written to `out`.
pub struct DigestLog<W> {
    out: W,
    seq: i64,
    /// The coinstats index's state for the active chain, from before genesis (D34) or from a seed.
    stats: CoinStatsAccumulator,
    /// The block `stats` is at: `None` before genesis.
    stats_tip: Option<BlockId>,
    /// The blocks whose `muhash` is finalized.
    muhash_at: MuHashAt,
    /// A record has failed: nothing more is written (O8's patch's `g_failed`, §8.3).
    failed: bool,
    observer: Option<Box<dyn RewindObserver>>,
}

/// The blocks at which the log finalizes the accumulator's MuHash (CH3a D165): for a `C`, its
/// own block; for a rewind, the disconnected block's parent. A finalize is most of a `C`'s cost
/// (CH3 R11), and Core's index keeps the hash of every block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MuHashAt {
    /// Every block: the golden's `muhash` at every height.
    Every,
    /// The blocks at these heights only.
    Only(BTreeSet<u32>),
}

impl MuHashAt {
    /// `stats`' MuHash, finalized, if this picks `height`.
    fn finalize(&self, height: u32, stats: &mut CoinStatsAccumulator) -> Option<Hash256> {
        let picked = match self {
            MuHashAt::Every => true,
            MuHashAt::Only(heights) => heights.contains(&height),
        };
        picked.then(|| Hash256::from_wire_bytes(stats.finalize()))
    }
}

/// The accumulator after a clean rewind, at the disconnected block's parent: the golden keys
/// (O8 §5.3) `gettxoutsetinfo muhash` would give there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewound {
    /// The parent's height and hash.
    pub height: u32,
    pub hash: Hash256,
    /// `None` where [`MuHashAt`] does not pick the parent.
    pub muhash: Option<Hash256>,
    pub txouts: u64,
    pub amount: i64,
}

/// Sees each clean rewind's [`Rewound`] (CH3a D152).
pub trait RewindObserver: Send {
    /// Called after the rewind, and after the `D` is written.
    ///
    /// # Errors
    ///
    /// The observer's verdict on the values. It fails the record as a failed write does: an
    /// `AbortNode`, while the attempt keeps its outcome (CH3a D137).
    fn rewound(&mut self, to: &Rewound) -> Result<(), Fatal>;
}

impl<W: Write + Send> DigestLog<W> {
    /// A log that has written `header` to `out`: it must then see every attempt from genesis on,
    /// since its golden keys accumulate from there.
    ///
    /// # Errors
    ///
    /// A failed write of the `H` record.
    pub fn new(out: W, header: Header) -> Result<DigestLog<W>, Fatal> {
        Self::open(out, header, CoinStatsAccumulator::new(), None)
    }

    /// A log that has written `header` to `out`, whose accumulator is `stats` at the block `at`
    /// (CH3a D165): a pruned start's base, its MuHash state from a scan of the base's coins. It
    /// must then see every attempt from `at`'s child on. Its `muhash` is the set's from there on,
    /// and its `txouts`, `amount` and `unspendable` are `stats`' plus the blocks'. `bi` is a
    /// difference of two totals, and its unclaimed rewards read only the index's balance,
    /// `prevout_spent + subsidy − (new_outputs_ex_coinbase + coinbase + unspendable)`, which is 0
    /// after every block (`index/coinstatsindex.cpp:208-210`, `:482-484`): `stats` must keep it 0,
    /// and its other totals are then free.
    ///
    /// # Errors
    ///
    /// A failed write of the `H` record.
    pub fn seeded(
        out: W,
        header: Header,
        stats: CoinStatsAccumulator,
        at: BlockId,
    ) -> Result<DigestLog<W>, Fatal> {
        Self::open(out, header, stats, Some(at))
    }

    fn open(
        mut out: W,
        header: Header,
        stats: CoinStatsAccumulator,
        stats_tip: Option<BlockId>,
    ) -> Result<DigestLog<W>, Fatal> {
        write_line(&mut out, &Record::Header(header).to_line())?;
        Ok(DigestLog {
            out,
            seq: 0,
            stats,
            stats_tip,
            muhash_at: MuHashAt::Every,
            failed: false,
            observer: None,
        })
    }

    /// The log, finalizing the MuHash at `at`'s blocks only.
    #[must_use]
    pub fn with_muhash_at(mut self, at: MuHashAt) -> DigestLog<W> {
        self.muhash_at = at;
        self
    }

    /// The log, with `observer` seeing every clean rewind from now on.
    #[must_use]
    pub fn with_observer(mut self, observer: Box<dyn RewindObserver>) -> DigestLog<W> {
        self.observer = Some(observer);
        self
    }
}

/// The record's identity: the block and the attempt's origin.
fn ident(cx: &ConnectCx<'_>, at: &BlockAt) -> Result<Ident, Fatal> {
    Ok(Ident {
        h: at.height.into(),
        b: display(at.hash)?,
        org: origin(cx.origin),
    })
}

fn shape(e: dash_statedigest::FieldError) -> Fatal {
    Fatal::new(format!("statedigest: {e}"))
}

fn undo_record(bytes: &[u8], at: u32) -> Result<BlockUndoRef<'_>, Fatal> {
    // A prefix: `Rewind::undo` ends with the checksum `read_undo_record` verified before `Ok`.
    BlockUndoRef::decode(&mut &bytes[..])
        .map_err(|e| Fatal::new(format!("statedigest: the undo at {at} does not decode: {e:?}")))
}

fn missing(what: &str, at: u32) -> Fatal {
    Fatal::new(format!("statedigest: no {what} for the block at {at}"))
}

/// The derived keys of the block `id` at `h` (O8 §5.2): the deployment answers for its child. A
/// view without versionbits answers has no `dep` to give.
// Core: statedigest.cpp's AttemptScope::Connected (O8's patch, node/state-digest-v24.0)
fn derived(
    cx: &ConnectCx<'_>,
    id: BlockId,
    h: u32,
    undo: Option<&[u8]>,
    c: &mut Outcome,
) -> Result<(), Fatal> {
    let after = |d| cx.deployments.active_after(Some(id), Deployment::Buried(d));
    let bur = BURIED.iter().enumerate().filter(|(_, d)| after(**d)).fold(0, |m, (i, _)| m | 1 << i);
    let rows: Vec<DepRow<'_>> = VERSIONBITS
        .iter()
        .map(|&(pos, name)| {
            cx.deployments.versionbits(Some(id), pos).map(|a| DepRow {
                name,
                state: a.state.name(),
                since: a.since,
            })
        })
        .collect::<Option<_>>()
        .ok_or_else(|| missing("versionbits answers", h))?;
    let fields = DerivedFields {
        undo: undo.map(|u| Hash256::from_wire_bytes(dash_sha256::sha256d(u))),
        bls: !after(BuriedDeployment::V19),
        bur,
        dep: Some(rows.as_slice()),
    };
    with_derived(c, &fields).map_err(shape)
}

fn stats_row(s: &BlockStats) -> [i64; 7] {
    [s.subsidy, s.totalfee, s.txs, s.ins, s.outs, s.utxo_increase, s.utxo_size_inc]
}

impl<W: Write + Send> DigestLog<W> {
    /// The golden keys of a block connected to the active coins (O8 §5.3), after its
    /// `CustomAppend` (`index/coinstatsindex.cpp:117-234`): `gettxoutsetinfo muhash <h>` and
    /// `getblockstats <h>` at the block. The accumulator must be at the block's parent, as the
    /// index checks its last entry is (`:133-139`): a log that missed a block has no golden keys
    /// to give.
    fn golden(&mut self, c: &ConnectAttempt<'_>) -> Result<GoldenFields, Fatal> {
        let (h, block) = (c.at.height, c.block);
        if c.at.prev != self.stats_tip {
            return Err(Fatal::new(format!(
                "statedigest: the coin stats are not at the parent of the block at {h}: the log \
                 missed a block"
            )));
        }
        let subsidy = c.subsidy.ok_or_else(|| missing("subsidy", h))?;
        let prev = *self.stats.totals();
        let failed = |e| Fatal::new(format!("statedigest: coin stats at {h}: {e}"));
        let stats = if c.at.prev.is_none() {
            // Genesis: the index reads no undo record (`:127-128`), and the golden has no stats.
            self.stats.append_block(0, block, &BlockUndo::default(), subsidy).map_err(failed)?;
            None
        } else {
            let undo = undo_record(c.undo.ok_or_else(|| missing("undo", h))?, h)?;
            let stats = block_stats(block, &undo, subsidy).map_err(failed)?;
            self.stats.append_block(h, block, &undo, subsidy).map_err(failed)?;
            Some(stats_row(&stats))
        };
        self.stats_tip = c.at.id;
        let muhash = self.muhash_at.finalize(h, &mut self.stats);
        let t = self.stats.totals();
        let info = t.block_info(c.at.prev.map(|_| &prev));
        Ok(GoldenFields {
            muhash,
            txouts: t.transaction_output_count,
            amount: t.total_amount,
            unspendable: t.total_unspendable_amount,
            bi: [
                info.prevout_spent,
                info.coinbase,
                info.new_outputs_ex_coinbase,
                info.unspendable,
                info.unspendables_genesis_block,
                info.unspendables_scripts,
                info.unspendables_unclaimed_rewards,
            ],
            stats,
        })
    }

    fn connect_record(
        &mut self,
        cx: &ConnectCx<'_>,
        c: &ConnectAttempt<'_>,
        seq: i64,
    ) -> Result<Outcome, Fatal> {
        let id = ident(cx, c.at)?;
        let reject = |result, dbg: Option<&str>| {
            emit::reject(id.clone(), result, dbg.filter(|d| !d.is_empty()), seq).map_err(shape)
        };
        match c.outcome {
            Ok(ConnectOutcome::Connected) if !c.just_check => {
                let block_id =
                    c.at.id.ok_or_else(|| Fatal::new("statedigest: a C without an index"))?;
                // Only genesis's `C` has a `null` undo (O8 §5.2).
                if c.at.prev.is_some() && c.undo.is_none() {
                    return Err(missing("undo", c.at.height));
                }
                // The connect may have raised the entry's status, so it is read now.
                let header = header_fields(cx, c.block, &entry(cx, block_id)?)?;
                let mut record = emit::connect(id.clone(), &header, seq).map_err(shape)?;
                derived(cx, block_id, c.at.height, c.undo, &mut record)?;
                if c.active {
                    let golden = self.golden(c)?;
                    with_golden(&mut record, &golden).map_err(shape)?;
                }
                Ok(record)
            }
            // A `fJustCheck` attempt that passed.
            Ok(ConnectOutcome::Connected) => reject(RejectResult::Ok, None),
            Ok(ConnectOutcome::Invalid(e)) => reject(
                RejectResult::Invalid {
                    cls: class_name(e.result()),
                    why: e.reject_reason(),
                },
                Some(e.debug_message()),
            ),
            // `state.Error(msg)`: the reason is the message, and there is no debug message.
            Ok(ConnectOutcome::SystemError(why)) => reject(
                RejectResult::Error {
                    why: if why.is_empty() {
                        "unset"
                    } else {
                        why
                    },
                },
                None,
            ),
            // No verdict: Core's counterpart throws out of `ConnectBlock` (CH3a D138).
            Err(f) => reject(
                RejectResult::Error {
                    why: "exception",
                },
                Some(&f.0),
            ),
        }
    }

    // Core: index/coinstatsindex.cpp:403-505 CoinStatsIndex::ReverseBlock
    /// The coinstats index's rewind of a block the active coins disconnected cleanly. The
    /// accumulator must be at the block, as the index checks its parent's entry (`:417-422`).
    fn rewind(&mut self, at: &BlockAt, block: &Block, r: &Rewind<'_>) -> Result<(), Fatal> {
        let h = at.height;
        if at.id != self.stats_tip {
            return Err(Fatal::new(format!(
                "statedigest: the coin stats are not at the block at {h} the disconnect rewinds: \
                 the log missed a block"
            )));
        }
        let undo = undo_record(r.undo, h)?;
        self.stats
            .reverse_block(h, block, &undo, r.subsidy)
            .map_err(|e| Fatal::new(format!("statedigest: coin stats rewind at {h}: {e}")))?;
        self.stats_tip = at.prev;
        let Some(observer) = &mut self.observer else {
            return Ok(());
        };
        let parent = h.checked_sub(1).ok_or_else(|| Fatal::new("statedigest: genesis rewound"))?;
        let t = self.stats.totals();
        let (txouts, amount) = (t.transaction_output_count, t.total_amount);
        let muhash = self.muhash_at.finalize(parent, &mut self.stats);
        observer.rewound(&Rewound {
            height: parent,
            hash: display(block.header().prev_hash())?,
            muhash,
            txouts,
            amount,
        })
    }
}

impl<W: Write + Send> DigestSink for DigestLog<W> {
    // Core: statedigest.cpp's Fail and WriteLine (O8's patch, node/state-digest-v24.0)
    /// After a failed record the log writes nothing more and reports nothing more: its failure
    /// was the one `AbortNode`.
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        if self.failed {
            return Ok(());
        }
        let recorded = self.write(cx, attempt);
        self.failed = recorded.is_err();
        recorded
    }
}

impl<W: Write + Send> DigestLog<W> {
    fn write(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        self.seq += 1;
        let seq = self.seq;
        match attempt {
            DigestAttempt::Connect(c) => {
                let record = self.connect_record(cx, c, seq)?;
                write_line(&mut self.out, &Record::Outcome(record).to_line())
            }
            DigestAttempt::Disconnect(d) => {
                // An error escaping `DisconnectBlock` is `failed` too (§4.4, CH3a D133).
                let record = emit::disconnect(ident(cx, d.at)?, disconnect_res(d.outcome), seq)
                    .map_err(shape)?;
                write_line(&mut self.out, &Record::Outcome(record).to_line())?;
                if let Some(r) = &d.rewind {
                    self.rewind(d.at, d.block, r)?;
                }
                Ok(())
            }
        }
    }
}
