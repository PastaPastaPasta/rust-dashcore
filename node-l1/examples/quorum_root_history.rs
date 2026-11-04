//! LQ2 D15's `quorum_root_history`: "the quorum root equals CbTx over all history". Connects the
//! active chain of a Core blocks directory in order through `commitments_from_block` and the
//! record path of `ProcessCommitment` (`mined_record`, into a `MemEvoStore`; no BLS, no members:
//! the CbTx check needs only the mined index), and at every block whose CbTx version is at least
//! 2 compares `quorum_root` with the CbTx's `merkleRootQuorums`. One `QcHashCache` serves every
//! root, dropped at each block that writes a commitment, as `ProcessCommitment` drops Core's.
//!
//! `cargo run --release -p node-l1 --example quorum_root_history -- --network mainnet --blocks
//! <datadir>/blocks [--tip <hash>]`
//!
//! `--network regtest` takes the functional test's chain options as `--args "-dip3params=2:2
//! -testactivationheight=v20@100 ..."`. Without `--tip` the tip is the one deepest block of the
//! directory. Prints the per-chain counts and every mismatch (height, ours, Core's); exits 1 on a
//! mismatch, 2 on an error. `node-l1/tests/l1_lq2_qroot.rs` runs [`replay`] over the regtest-v24
//! conformance chains.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::primitives::Block;
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx, decode_block};
use dash_coreimport::blockfiles::{BlockReader, HeaderIndex};
use dash_coreimport::{Hash256, hash_from_hex, hash_to_hex};
use dash_dml::blockprocessor::{QcHashCache, commitments_from_block, mined_record, quorum_root};
use dash_dml::store::{EvoDelta, MemEvoStore, StoreError};
use dash_evo::bls_ctx_for_block;
use dash_evo::cbtx::CBTX_VERSION_MERKLE_ROOT_QUORUMS;
use dash_evo::llmq::{CompleteChain, LlmqCtx};
use dash_num::{Arith256, CompactTarget};
use dash_params::{
    BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, Network, Release,
};
use dash_primitives::{BlockHash, CoinbaseCommitment, SpecialPayload, TxType};
use dash_types::Numeric;

/// One block whose computed root differs from its CbTx's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub height: u32,
    /// Our root, or the reject reason `CalcCbTxMerkleRootQuorums` would give.
    pub ours: String,
    /// The CbTx's `merkleRootQuorums`.
    pub core: String,
}

/// What a replay saw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The tip, display form.
    pub tip: String,
    /// Blocks connected, genesis included.
    pub blocks: u32,
    /// Blocks whose CbTx version is at least 2: the roots compared.
    pub checked: u32,
    /// Blocks with a CbTx of version 1 (not compared).
    pub cbtx_v1: u32,
    /// Blocks after genesis without a CbTx (before DIP3, or connected under other parameters, as
    /// a functional test's cached chain is).
    pub no_cbtx: u32,
    /// The first checked height.
    pub first_checked: Option<u32>,
    /// Non-null commitments indexed, by (type, rotated).
    pub commitments: BTreeMap<(u8, bool), u32>,
    /// Null commitments seen (not stored).
    pub null_commitments: u32,
    /// Checked blocks whose root covers at least one non-null mined commitment.
    pub checked_nonempty: u32,
    pub mismatches: Vec<Mismatch>,
}

impl Report {
    /// Non-null commitments indexed, every type.
    pub fn nonnull(&self) -> u32 {
        self.commitments.values().sum()
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tip {} height {}: {} blocks, {} CbTx v2+ roots checked (from {}; {} with a non-null \
             commitment indexed), {} CbTx v1 and {} without a CbTx skipped, {} non-null \
             commitments indexed",
            self.tip,
            self.blocks.saturating_sub(1),
            self.blocks,
            self.checked,
            self.first_checked.map_or_else(|| "-".to_owned(), |h| h.to_string()),
            self.checked_nonempty,
            self.cbtx_v1,
            self.no_cbtx,
            self.nonnull(),
        )?;
        for ((t, rotated), n) in &self.commitments {
            write!(
                f,
                " [type {t} {}: {n}]",
                if *rotated {
                    "rotated"
                } else {
                    "plain"
                }
            )?;
        }
        write!(f, ", {} null, {} mismatches", self.null_commitments, self.mismatches.len())
    }
}

/// The active chain as Core's block index: `BlockId` is the height.
struct ActiveChain {
    headers: Vec<HeaderCtx>,
}

impl ChainAncestry for ActiveChain {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.headers[id.0 as usize]
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// The buried deployments at their heights. The replay reads only buried ones (DIP3 in
/// `GetCommitmentsFromBlock` and `ProcessBlock`, DIP24 in `IsQuorumRotationEnabled`), which Core
/// decides by height alone (`deploymentstatus.h` rc.3:14-18, :34-38).
struct Buried<'a> {
    params: &'a ChainSetup,
}

impl DeploymentView for Buried<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| p.0 + 1);
        match deployment {
            Deployment::Buried(d) => next >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => unreachable!("the replay reads buried deployments only"),
        }
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(block.0.checked_sub(1).map(BlockId), deployment)
    }
}

/// The chain parameters Core builds for `network` from `args` (`-name=value` each).
pub fn params_of(network: Network, args: &[String]) -> Result<ChainSetup, String> {
    let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
    for arg in args {
        let (name, value) = arg.split_once('=').ok_or_else(|| format!("{arg}: not -name=value"))?;
        let option =
            ChainOption::from_name(name).ok_or_else(|| format!("{name}: not a chain option"))?;
        builder = builder.arg(option, value);
    }
    builder.build().map_err(|e| format!("chain parameters: {e:?}"))
}

/// The CbTx of a block, as `ProcessSpecialTxsInBlock` reads it: `vtx[0]` of type
/// `TRANSACTION_COINBASE` whose payload `GetTxPayload` reads whole (`evo/specialtxman.cpp`
/// rc.3:816-833).
fn cbtx(block: &Block) -> Result<Option<&CoinbaseCommitment>, String> {
    let Some(tx) = block.txs().first() else {
        return Err("a block without transactions".into());
    };
    if tx.tx_type() != TxType::CoinbaseCommitment {
        return Ok(None);
    }
    match tx.payload() {
        Some(Ok(decoded)) if decoded.is_complete() => match &decoded.payload {
            SpecialPayload::CoinbaseCommitment(cbtx) => Ok(Some(cbtx)),
            _ => Err("bad-cbtx-payload".into()),
        },
        _ => Err("bad-cbtx-payload".into()),
    }
}

/// The deepest block of `index`, when exactly one block is deepest.
fn deepest(index: &HeaderIndex) -> Result<Hash256, String> {
    let mut height: HashMap<Hash256, u32> = HashMap::with_capacity(index.blocks.len());
    for start in index.blocks.keys() {
        // Walk down to a block of known height (or genesis), then fill the path.
        let mut path = Vec::new();
        let mut cur = *start;
        let mut base = loop {
            if let Some(h) = height.get(&cur) {
                break Some(*h);
            }
            let Some(loc) = index.blocks.get(&cur) else {
                return Err(format!("block {} has no parent in the files", hash_to_hex(&cur)));
            };
            path.push(cur);
            if loc.prev == [0; 32] {
                break None;
            }
            cur = loc.prev;
        };
        for hash in path.into_iter().rev() {
            let h = base.map_or(0, |b| b + 1);
            height.insert(hash, h);
            base = Some(h);
        }
    }
    let max = height.values().copied().max().ok_or("an empty blocks directory")?;
    let mut tips = height.iter().filter(|(_, h)| **h == max).map(|(hash, _)| *hash);
    match (tips.next(), tips.next()) {
        (Some(tip), None) => Ok(tip),
        _ => Err(format!("several blocks at the greatest height {max}: pass --tip")),
    }
}

/// Replays the active chain ending at `tip` (default: the one deepest block) of the blocks
/// directory `dir` and compares every CbTx v2+ `merkleRootQuorums` with [`quorum_root`].
pub fn replay(params: &ChainSetup, dir: &Path, tip: Option<Hash256>) -> Result<Report, String> {
    let index = HeaderIndex::build(dir, params.message_start).map_err(|e| e.to_string())?;
    let tip = match tip {
        Some(tip) => tip,
        None => deepest(&index)?,
    };
    let blocks = index.chain_to(&tip).map_err(|e| e.to_string())?;
    let mut reader = BlockReader::new(&index.files);

    let mut chain = ActiveChain {
        headers: Vec::with_capacity(blocks.len()),
    };
    let mut height_of: HashMap<Hash256, u32> = HashMap::with_capacity(blocks.len());
    let mut buf = Vec::new();
    for (height, (hash, loc)) in (0u32..).zip(&blocks) {
        reader.read(loc, &mut buf).map_err(|e| e.to_string())?;
        let field =
            |at: usize| u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
        // Nothing here reads the median time past or the chain work.
        chain.headers.push(HeaderCtx {
            height,
            hash: BlockHash::from(*hash),
            time: field(68),
            bits: CompactTarget::new(field(72)),
            version: field(0) as i32,
            mtp: 0,
            chain_work: Arith256::ZERO,
        });
        height_of.insert(*hash, height);
    }

    let view: CompleteChain<'_, StoreError> = CompleteChain::new(&chain);
    let deployments = Buried {
        params,
    };
    let dip3 = Deployment::Buried(BuriedDeployment::Dip0003);
    let mut store = MemEvoStore::new();
    let mut hashes = QcHashCache::new();
    let mut report = Report {
        tip: hash_to_hex(&tip),
        ..Report::default()
    };
    for (height, (hash, loc)) in (0u32..).zip(&blocks) {
        reader.read(loc, &mut buf).map_err(|e| e.to_string())?;
        let block =
            decode_block(&mut &buf[..]).map_err(|e| format!("block {height}: {e:?}"))?.into_value();
        let id = BlockId(height);
        let llmq = LlmqCtx::new(params, &view, &deployments, bls_ctx_for_block(params, height))
            .map_err(|e| format!("{e:?}"))?;

        // `ProcessBlock` first (`specialtxman.cpp` rc.3:895): below DIP3 it writes nothing
        // (`llmq/blockprocessor.cpp` rc.3:218-221); above, each non-null commitment's rows
        // (:421-441), nulls none (:366-379).
        let mut mined = Vec::new();
        if deployments.active_at(id, dip3) {
            let qcs = commitments_from_block(&llmq, &block, id)
                .map_err(|e| format!("block {height}: {e}"))?;
            for qc in &qcs {
                if qc.is_null() {
                    report.null_commitments += 1;
                    continue;
                }
                let base_hash = *qc.quorum_hash.as_bytes();
                let Some(&base_height) = height_of.get(&base_hash) else {
                    return Err(format!(
                        "block {height}: quorum hash {} is not on the active chain",
                        hash_to_hex(&base_hash)
                    ));
                };
                let params = params
                    .consensus
                    .llmq(qc.llmq_type)
                    .ok_or_else(|| format!("block {height}: an unconfigured type"))?;
                let record = mined_record(&llmq, params, qc, BlockId(base_height), base_height)
                    .map_err(|e| format!("block {height}: {e}"))?;
                *report
                    .commitments
                    .entry((qc.llmq_type.to_base(), record.rotation_index.is_some()))
                    .or_default() += 1;
                mined.push(record);
            }
        }
        if !mined.is_empty() {
            // `DropQcHashesCache` (`llmq/blockprocessor.cpp` rc.3:446).
            hashes.clear();
        }
        store
            .apply(&EvoDelta {
                block: BlockHash::from(*hash),
                height,
                dml: None,
                mined_commitments: mined,
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: None,
            })
            .map_err(|e| format!("block {height}: {e}"))?;
        report.blocks += 1;

        // Then the CbTx roots, from version 2 (`specialtxman.cpp` rc.3:943-952), the quorums'
        // over `pindex->pprev` plus the block's own commitments (`evo/cbtx.cpp` rc.3:65-156),
        // so read through the parent, not this block.
        let Some(cbtx) = cbtx(&block).map_err(|e| format!("block {height}: {e}"))? else {
            report.no_cbtx += u32::from(height > 0);
            continue;
        };
        if cbtx.version < CBTX_VERSION_MERKLE_ROOT_QUORUMS {
            report.cbtx_v1 += 1;
            continue;
        }
        let Some(prev) = height.checked_sub(1).map(BlockId) else {
            return Err("a CbTx v2 in the genesis block".into());
        };
        let core = cbtx
            .merkle_root_quorums
            .ok_or_else(|| format!("block {height}: a CbTx v2 without merkleRootQuorums"))?;
        let evo = store.view(&chain);
        let ours = quorum_root(&llmq, &evo, &mut hashes, &block, prev)
            .map_err(|e| format!("block {height}: {e}"))?;
        report.checked += 1;
        report.first_checked.get_or_insert(height);
        if report.nonnull() > 0 {
            report.checked_nonempty += 1;
        }
        if ours.as_ref() != Ok(&core) {
            report.mismatches.push(Mismatch {
                height,
                ours: match ours {
                    Ok(root) => hash_to_hex(root.as_bytes()),
                    Err(e) => e.reject_reason().to_owned(),
                },
                core: hash_to_hex(core.as_bytes()),
            });
        }
    }
    Ok(report)
}

const USAGE: &str = "usage: quorum_root_history --network {mainnet,testnet,regtest} --blocks <dir> \
                     [--args \"-name=value ...\"] [--tip <hash>]";

fn main() -> ExitCode {
    let (mut network, mut blocks, mut args, mut tip) = (None, None, Vec::new(), None);
    let mut argv = std::env::args().skip(1);
    while let Some(flag) = argv.next() {
        let Some(value) = argv.next() else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        match flag.as_str() {
            "--network" => {
                network = match value.as_str() {
                    "mainnet" => Some(Network::Main),
                    "testnet" => Some(Network::Testnet3),
                    "regtest" => Some(Network::Regtest),
                    _ => None,
                }
            }
            "--blocks" => blocks = Some(PathBuf::from(value)),
            "--args" => args.extend(value.split_whitespace().map(str::to_owned)),
            "--tip" => match hash_from_hex(&value) {
                Some(hash) => tip = Some(hash),
                None => {
                    eprintln!("--tip {value}: not a block hash");
                    return ExitCode::from(2);
                }
            },
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(network), Some(blocks)) = (network, blocks) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let result = params_of(network, &args).and_then(|params| replay(&params, &blocks, tip));
    match result {
        Ok(report) => {
            println!("{report}");
            for m in &report.mismatches {
                println!("mismatch at {}: ours {}, Core's {}", m.height, m.ours, m.core);
            }
            if report.mismatches.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("quorum_root_history: {e}");
            ExitCode::from(2)
        }
    }
}
