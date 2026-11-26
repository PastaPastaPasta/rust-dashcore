//! The template's fields (`rpc/mining.cpp:835-982` at v24.0.2), from one V query turn after the
//! cache step.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use dash_consensus::ctx::{BlockId, Deployment, DeploymentPos, DeploymentView as _};
use dash_consensus::deployments::ThresholdState;
use dash_consensus::pow::{PowParams, get_next_work_required};
use dash_consensus::primitives::{TxHash, TxOut, TxRef};
use dash_consensus::tx::{max_block_sigops, max_block_size};
use dash_num::{Arith256, CompactTarget};
use dash_params::BuriedDeployment;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dashd_chainstate::chain::ChainQuery;

use super::cache::GbtCache;
use super::{FieldsAt, FieldsTurn, GbtError, GbtParts, TipFields, VbDeployment};
use crate::TemplateError;

/// The typed template, in Core's push order (`rpc/mining.cpp:885-982`); the RPC layer encodes
/// it and adds the constant fields (`capabilities`, `coinbaseaux`, `mutable`, `noncerange`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GbtTemplate {
    /// `nVersion` after the version-bits loop.
    pub version: i32,
    /// `"csv"`, then each active deployment's `gbt_vb_name`.
    pub rules: Vec<String>,
    /// Each started or locked-in deployment's `gbt_vb_name` and bit.
    pub vbavailable: Vec<(String, u8)>,
    pub vbrequired: i32,
    pub previousblockhash: BlockHash,
    /// Every transaction but the coinbase.
    pub transactions: Vec<GbtTx>,
    /// `vtx[0]->GetValueOut()`.
    pub coinbasevalue: i64,
    /// V's tip at the reply and `nTransactionsUpdatedLast` (`rpc/mining.cpp:938`).
    pub longpollid: (BlockHash, u32),
    /// `arith_uint256().SetCompact(nBits)`.
    pub target: Arith256,
    /// `pindexPrev->GetMedianTimePast() + 1`.
    pub mintime: i64,
    pub sigoplimit: u64,
    pub sizelimit: u64,
    /// `pblock->GetBlockTime()`.
    pub curtime: i64,
    pub bits: u32,
    pub previousbits: u32,
    /// `pindexPrev->nHeight + 1`.
    pub height: u32,
    pub masternode: Vec<TxOut>,
    pub masternode_payments_started: bool,
    pub masternode_payments_enforced: bool,
    pub superblock: Vec<TxOut>,
    pub superblocks_started: bool,
    pub superblocks_enabled: bool,
    /// The coinbase's `vExtraPayload`.
    pub coinbase_payload: Vec<u8>,
}

/// One `transactions` entry (`rpc/mining.cpp:850-870`).
#[derive(Clone)]
pub struct GbtTx {
    pub tx: TxRef,
    pub txid: TxHash,
    /// `setTxIndex[prevout.hash]` for each input whose hash it holds, in input order.
    pub depends: Vec<u32>,
    /// `vTxFees[i]`.
    pub fee: i64,
    /// `vTxSigOps[i]`.
    pub sigops: i64,
}

impl fmt::Debug for GbtTx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GbtTx")
            .field("txid", &self.txid)
            .field("depends", &self.depends)
            .field("fee", &self.fee)
            .field("sigops", &self.sigops)
            .finish_non_exhaustive()
    }
}

/// Equal entries hold the same serialized transaction.
impl PartialEq for GbtTx {
    fn eq(&self, other: &Self) -> bool {
        self.tx.instance_hash() == other.tx.instance_hash()
            && (&self.txid, &self.depends, self.fee, self.sigops)
                == (&other.txid, &other.depends, other.fee, other.sigops)
    }
}

impl Eq for GbtTx {}

/// [`answer`]'s outcome.
pub(super) enum Answer {
    Template(Box<GbtTemplate>),
    /// V's tip moved off the cached template's parent.
    Moved(BlockHash),
}

/// `UpdateTime`'s `nTime` (`node/miner.cpp:49-56` at v24.0.2): `max(MTP + 1, now)` when that is
/// later than `old`, else `old`. The store truncates to `uint32_t`, as Core's does.
pub fn update_time(old: u32, mtp: i64, now: i64) -> u32 {
    let new_time = (mtp + 1).max(now);
    if i64::from(old) < new_time {
        new_time as u32
    } else {
        old
    }
}

/// `gbt_vb_name` (`rpc/mining.cpp:550-557` at v24.0.2): the name, `!`-prefixed unless the
/// deployment is `gbt_force`.
pub fn gbt_vb_name(name: &str, gbt_force: bool) -> String {
    if gbt_force {
        name.to_owned()
    } else {
        format!("!{name}")
    }
}

/// V's side of the fields turn: [`FieldsTurn::Moved`] unless the tip is `at.prev`, else what the
/// fields read at it, `UpdateTime` included.
pub(super) fn tip_fields(
    q: &ChainQuery<'_>,
    at: FieldsAt,
    pow: &PowParams,
    allow_min_difficulty: bool,
    positions: &[DeploymentPos],
) -> Result<FieldsTurn, TemplateError> {
    let (tip, _) = q.chain.private_tip().ok_or(TemplateError::NoChain)?;
    if tip.hash != at.prev {
        return Ok(FieldsTurn::Moved(tip.hash));
    }
    let chain = q
        .chain
        .complete()
        .ok_or_else(|| TemplateError::Failed("V's chain has a header window".to_owned()))?;
    let mtp = chain.header(tip.id).mtp;
    // Core: v24.0.2 node/miner.cpp:49-61 UpdateTime
    let time = update_time(at.time, mtp, at.now);
    let min_difficulty_bits =
        allow_min_difficulty.then(|| get_next_work_required(chain, tip.id, time, pow).to_base());
    Ok(FieldsTurn::At(TipFields {
        mtp,
        time,
        min_difficulty_bits,
        versionbits: versionbits(q, tip.id, positions)?,
        dip0001: q
            .deployments
            .active_after(Some(tip.id), Deployment::Buried(BuriedDeployment::Dip0001)),
    }))
}

fn versionbits(
    q: &ChainQuery<'_>,
    prev: BlockId,
    positions: &[DeploymentPos],
) -> Result<Vec<(DeploymentPos, ThresholdState)>, TemplateError> {
    let mut states = Vec::with_capacity(positions.len());
    for &pos in positions {
        let answer = q
            .deployments
            .try_versionbits(Some(prev), pos)
            .map_err(|e| TemplateError::Failed(e.to_string()))?;
        if let Some(answer) = answer {
            states.push((pos, answer.state));
        }
    }
    Ok(states)
}

/// The fields of the cached template (`rpc/mining.cpp:835-982`), its in-place edits kept for
/// the next call as Core's are: `UpdateTime`, `nNonce = 0` (`:836-837`) and the version-bits
/// loop's (`:886-931`), up to a [`GbtError::RuleRequired`] it throws.
pub(super) fn answer(
    cache: &mut GbtCache,
    parts: &GbtParts,
    now: i64,
    client_rules: &BTreeSet<String>,
) -> Result<Answer, GbtError> {
    let tx_updated_last = cache.tx_updated_last;
    let cached = cache.cached();
    let prev = cached.template.prev;
    let params = &parts.params;
    let fields = match parts.chain.fields_turn(FieldsAt {
        prev,
        time: cached.time,
        now,
    })? {
        FieldsTurn::Moved(tip) => return Ok(Answer::Moved(tip)),
        FieldsTurn::At(fields) => fields,
    };
    cached.time = fields.time;
    if let Some(bits) = fields.min_difficulty_bits {
        cached.bits = bits;
    }
    cached.nonce = 0;
    let transactions = transactions(&cached.template);
    let VersionBits {
        rules,
        vbavailable,
    } = version_bits(&mut cached.version, &params.deployments, &fields.versionbits, client_rules)?;
    let template = &cached.template;
    let coinbase = &template.block.txs()[0];
    let height = template.height;
    Ok(Answer::Template(Box::new(GbtTemplate {
        version: cached.version,
        rules,
        vbavailable,
        vbrequired: 0,
        previousblockhash: template.block.header().prev_hash(),
        transactions,
        coinbasevalue: coinbase.value_out(),
        // :938: V's tip at the reply, which this turn found to be `prev`.
        longpollid: (prev, tx_updated_last),
        target: CompactTarget::new(cached.bits).expand().value,
        mintime: fields.mtp + 1,
        sigoplimit: u64::from(max_block_sigops(fields.dip0001)),
        sizelimit: u64::from(max_block_size(fields.dip0001)),
        curtime: i64::from(cached.time),
        bits: cached.bits,
        previousbits: template.prev_bits,
        height,
        masternode: template.masternode_payments.clone(),
        masternode_payments_started: started(height, params.masternode_payments_start),
        masternode_payments_enforced: true,
        superblock: template.superblock_payments.clone(),
        superblocks_started: started(height, params.superblocks.start()),
        superblocks_enabled: true,
        coinbase_payload: coinbase.extra_payload().to_vec(),
    })))
}

/// `pindexPrev->nHeight + 1 > start` (`rpc/mining.cpp:961`, `:977`).
fn started(height: u32, start: i32) -> bool {
    i64::from(height) > i64::from(start)
}

/// `transactions` (`rpc/mining.cpp:841-871`): `setTxIndex` takes each transaction's index as
/// the loop reaches it, the coinbase's 0 included, so an input finds only an earlier
/// transaction (or the one it is in); `depends` lists one index per such input, duplicates kept.
fn transactions(template: &crate::BlockTemplate) -> Vec<GbtTx> {
    let txs = template.block.txs();
    let mut index: HashMap<TxHash, u32> = HashMap::with_capacity(txs.len());
    let mut entries = Vec::with_capacity(txs.len().saturating_sub(1));
    for (i, tx) in txs.iter().enumerate() {
        let txid = tx.txid();
        index.insert(txid, i as u32);
        if i == 0 {
            continue;
        }
        let depends = tx
            .inputs()
            .iter()
            .filter_map(|input| index.get(&input.prevout().txid()).copied())
            .collect();
        entries.push(GbtTx {
            tx: tx.clone(),
            txid,
            depends,
            fee: template.tx_fees[i],
            sigops: template.tx_sigops[i],
        });
    }
    entries
}

/// The version-bits loop's `rules` and `vbavailable`.
struct VersionBits {
    rules: Vec<String>,
    vbavailable: Vec<(String, u8)>,
}

/// The version-bits loop (`rpc/mining.cpp:885-931`) over `deployments` in position order: it
/// edits `version` in place.
///
/// # Errors
///
/// [`GbtError::RuleRequired`] for an active deployment that is not `gbt_force` and that the
/// client did not list, after the edits of the deployments before it.
fn version_bits(
    version: &mut i32,
    deployments: &[VbDeployment],
    states: &[(DeploymentPos, ThresholdState)],
    client_rules: &BTreeSet<String>,
) -> Result<VersionBits, GbtError> {
    let mut rules = vec!["csv".to_owned()];
    let mut vbavailable = Vec::new();
    for d in deployments {
        let Some(&(_, state)) = states.iter().find(|(pos, _)| *pos == d.pos) else {
            continue;
        };
        let mask = (1u32 << d.bit) as i32;
        let listed = client_rules.contains(d.name);
        match state {
            ThresholdState::Defined | ThresholdState::Failed => {}
            ThresholdState::LockedIn | ThresholdState::Started => {
                if state == ThresholdState::LockedIn {
                    *version |= mask;
                }
                vbavailable.push((gbt_vb_name(d.name, d.gbt_force), d.bit));
                if !listed && !d.gbt_force {
                    *version &= !mask;
                }
            }
            ThresholdState::Active => {
                rules.push(gbt_vb_name(d.name, d.gbt_force));
                if !listed && !d.gbt_force {
                    return Err(GbtError::RuleRequired(d.name.to_owned()));
                }
            }
        }
    }
    Ok(VersionBits {
        rules,
        vbavailable,
    })
}
