//! LQ2 D14's `quorum listextended` digest: LQ1's `ScanQuorums` over LQ2's mined index, compared
//! with every `quorum_listextended` sample of an O8 StateDigest golden (Core-produced by
//! `-statedigestlog`).
//!
//! `cargo run --release -p node-l1 --example lq2_listextended -- <golden>` (needs no oracle)
//!
//! The golden (`golden.jsonl.zst`, read through `zstd -dc`, or plain JSONL) is replayed in height
//! order. Each `C` record extends the header chain with its block hash; its `s.qc` (Core's
//! per-block `q_mc` writes: type, quorum hash, `SerializeHash`) becomes the
//! `MinedCommitmentRecord`s `ProcessCommitment` writes, built by LQ2's [`mined_record`] (the
//! production decision: the indexed family iff rotation is enabled for the type at the base block)
//! and applied to a [`MemEvoStore`]. The base height is the height of the quorum hash's block in
//! the golden's own chain; the quorum index is the one `GetQuorumBlockHash` ties to it. The
//! deployments are the chain parameters' buried heights, checked against every record's `bur`.
//!
//! At each sample height `h` it computes what Core's RPC returns through the active chain at `h`
//! and compares, per enabled type, the quorums newest first: hash, `quorumIndex`, `creationHeight`
//! and `minedBlockHash`. `numValidMembers` and `healthRatio` are not compared (LQ2 D14): they read
//! `members.size()` and the commitment's bits, which the golden does not carry. The type order is
//! not compared either: the golden's canonical JSON sorts object keys.
//!
//! Prints the totals, the per-type counts and the first 20 mismatches in full; exits 1 on any
//! mismatch, 2 on an error.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, ExitCode, Stdio};
use std::time::Instant;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_dml::blockprocessor::mined_record;
use dash_dml::blockprocessor::pool::null_commitment;
use dash_dml::store::{EvoDelta, MemEvoStore, MemEvoView, StoreError};
use dash_evo::bls_ctx_for_block;
use dash_evo::llmq::{
    CompleteChain, LlmqCtx, LlmqError, MemberError, MinedCommitmentReader, QuorumMaterializer,
    ScanCache, ScanEntry, enabled_quorum_params, scan_quorums_from, scan_store_block,
};
use dash_num::{Arith256, CompactTarget};
use dash_params::LlmqType;
use dash_params::Release;
use dash_params::{BuriedDeployment, ChainParamsBuilder, ChainSetup, LlmqParams, Network};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use serde::Deserialize;
use serde_json::Value;

/// How many mismatches are printed in full.
const SHOWN: usize = 20;

/// Core's `Consensus::BuriedDeployment` in enum order: bit `i` of a `C` record's `bur` is the
/// `i`-th (O8 §5.2).
// Core: consensus/params.h:22-38 (v24.0.0-rc.3) enum BuriedDeployment
const CORE_BURIED: [BuriedDeployment; 15] = [
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

/// Buried deployments at the parameters' heights, on a chain whose `BlockId` is the height; no
/// versionbits deployment is active (no testnet LLMQ type reads one).
// Core: deploymentstatus.h:14 (v24.0.0-rc.3) DeploymentActiveAfter (buried)
struct HeightDeployments<'a> {
    params: &'a ChainSetup,
}

impl DeploymentView for HeightDeployments<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| p.0 + 1);
        match deployment {
            Deployment::Buried(d) => next >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_after(block.0.checked_sub(1).map(BlockId), deployment)
    }
}

/// One golden record, as far as this replay reads it.
#[derive(Deserialize)]
struct Record {
    k: String,
    #[serde(default)]
    h: Option<u32>,
    #[serde(default)]
    b: Option<String>,
    #[serde(default)]
    org: Option<String>,
    #[serde(default)]
    chain: Option<String>,
    #[serde(default)]
    ver: Option<String>,
    #[serde(default)]
    s: Option<State>,
    #[serde(default)]
    rpc: Option<String>,
    #[serde(default)]
    args: Option<Vec<Value>>,
    #[serde(default)]
    res: Option<Value>,
}

/// A `C` record's state, as far as this replay reads it.
#[derive(Deserialize)]
struct State {
    prev: Option<String>,
    /// `(type, quorum hash, SerializeHash(qc))` per `q_mc` write; `null` outside evo windows.
    qc: Option<Vec<(u8, String, String)>>,
    bur: u32,
}

/// The golden's active chain: `BlockId(h)` is the block at height `h`. The golden is one linear
/// chain (every `C` connects the tip and there is no `D`), which [`Replay::connect`] checks.
struct Line {
    hashes: Vec<BlockHash>,
}

impl ChainAncestry for Line {
    fn header(&self, id: BlockId) -> HeaderCtx {
        HeaderCtx {
            height: id.0,
            hash: self.hashes[id.0 as usize],
            time: 0,
            bits: CompactTarget::new(0),
            version: 0,
            mtp: 0,
            chain_work: Arith256::ZERO,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// A quorum as `listextended` reads it: `m_quorum_base_block_index`, `qc->quorumHash`,
/// `qc->quorumIndex` and `minedBlockHash`.
#[derive(Clone, Debug)]
struct Quorum {
    base: BlockId,
    quorum_hash: QuorumHash,
    quorum_index: i16,
    mined_block: BlockHash,
}

impl ScanEntry for Quorum {
    fn base(&self) -> BlockId {
        self.base
    }

    fn quorum_hash(&self) -> QuorumHash {
        self.quorum_hash
    }

    fn quorum_index(&self) -> i16 {
        self.quorum_index
    }
}

/// Core's `GetQuorum(type, base, populate_cache)` as far as `listextended` reads the quorum:
/// `HasQuorum` against the active chain (`nullptr` when not mined in it), then the mined
/// commitment and the block that mined it (`BuildQuorumFromCommitment`'s
/// `GetMinedCommitment`). The members are not built: the compared fields do not read them.
// Core: llmq/quorumsman.cpp:518 (v24.0.0-rc.3) CQuorumManager::GetQuorum
struct Quorums<'a> {
    view: MemEvoView<'a>,
    /// The active tip of the sample (Core's `HasQuorum` reads the active chain).
    tip: BlockId,
}

impl QuorumMaterializer for Quorums<'_> {
    type Quorum = Quorum;
    type Error = StoreError;

    fn get_quorum(
        &mut self,
        llmq_type: LlmqType,
        base: BlockId,
        _populate_cache: bool,
    ) -> Result<Option<Quorum>, LlmqError<StoreError>> {
        let quorum_hash = QuorumHash::from(*self.view.chain().header(base).hash.as_bytes());
        let mined = self
            .view
            .mined_commitment(llmq_type, &quorum_hash, self.tip)
            .map_err(MemberError::View)?;
        Ok(mined.map(|(qc, mined_block)| Quorum {
            base,
            quorum_hash: qc.quorum_hash,
            // `qc->quorumIndex`: 0 for a version that carries none.
            quorum_index: qc.quorum_index.unwrap_or(0),
            mined_block,
        }))
    }
}

/// One quorum of a `listextended` entry, in the compared fields.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    quorum_hash: String,
    /// Present for `useRotation` types only.
    quorum_index: Option<i64>,
    creation_height: i64,
    mined_block_hash: String,
}

impl std::fmt::Display for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{{{}: index {:?}, creation {}, mined {}}}",
            self.quorum_hash, self.quorum_index, self.creation_height, self.mined_block_hash
        )
    }
}

/// Per-type counts.
#[derive(Default)]
struct TypeCounts {
    /// Samples in which the type was listed (by Core or by us).
    samples: u64,
    /// Quorums compared (Core's entries).
    quorums: u64,
    /// Non-null commitments indexed.
    indexed: u64,
    /// Of those, in the indexed (rotated) family.
    rotated: u64,
}

#[derive(Default)]
struct Report {
    blocks: u64,
    samples: u64,
    quorums: u64,
    /// Records whose `bur` bits disagree with the parameters' buried heights.
    bur_mismatches: u64,
    per_type: BTreeMap<String, TypeCounts>,
    mismatches: u64,
    shown: Vec<String>,
}

impl Report {
    fn mismatch(&mut self, what: String) {
        self.mismatches += 1;
        if self.shown.len() < SHOWN {
            self.shown.push(what);
        }
    }
}

struct Replay {
    params: ChainSetup,
    chain: Line,
    heights: HashMap<BlockHash, u32>,
    store: MemEvoStore,
    /// Samples ahead of the chain, by height (none in the O8 goldens: Core writes a sample after
    /// its block).
    pending: BTreeMap<u32, Vec<Value>>,
    report: Report,
}

fn fail(what: impl std::fmt::Display) -> String {
    what.to_string()
}

/// A display-order (reversed) hex hash.
fn hash_from_display(hex: &str) -> Result<[u8; 32], String> {
    let bad = || format!("not a 32-byte hex hash: {hex}");
    if hex.len() != 64 {
        return Err(bad());
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2).ok_or_else(bad)?, 16)
            .map_err(|_| bad())?;
    }
    bytes.reverse();
    Ok(bytes)
}

fn display(bytes: &[u8; 32]) -> String {
    bytes.iter().rev().map(|b| format!("{b:02x}")).collect()
}

fn llmq_params(params: &ChainSetup, llmq_type: u8) -> Result<(LlmqType, &LlmqParams), String> {
    params
        .consensus
        .llmq_params()
        .find(|(t, _)| t.to_base() == llmq_type)
        .ok_or_else(|| format!("llmq type {llmq_type} is not configured"))
}

impl Replay {
    fn new(network: Network) -> Result<Self, String> {
        Ok(Replay {
            params: ChainParamsBuilder::new(network, Release::V24_0).build().map_err(fail)?,
            chain: Line {
                hashes: Vec::new(),
            },
            heights: HashMap::new(),
            store: MemEvoStore::new(),
            pending: BTreeMap::new(),
            report: Report::default(),
        })
    }

    /// The tip's height (the chain is not empty).
    fn tip(&self) -> u32 {
        (self.chain.hashes.len() - 1) as u32
    }

    /// A `C` record: extend the chain, check `bur`, index the block's `q_mc` writes.
    fn connect(&mut self, rec: Record) -> Result<(), String> {
        let height = rec.h.ok_or("a C record without h")?;
        let hash = BlockHash::from(hash_from_display(rec.b.as_deref().ok_or("a C without b")?)?);
        let state = rec.s.ok_or("a C record without s")?;
        if rec.org.as_deref() != Some("tip") {
            return Err(format!("C at {height}: origin {:?}, not the tip", rec.org));
        }
        // One linear chain: each C connects on the previous one.
        if height as usize != self.chain.hashes.len() {
            return Err(format!(
                "C at {height} after {} blocks (a reorg?)",
                self.chain.hashes.len()
            ));
        }
        let prev = state.prev.as_deref().map(hash_from_display).transpose()?.map(BlockHash::from);
        if prev != self.chain.hashes.last().copied() {
            return Err(format!("C at {height}: prev is not the previous block"));
        }
        self.chain.hashes.push(hash);
        self.heights.insert(hash, height);
        self.report.blocks += 1;

        // `bur` bit i: `DeploymentActiveAfter(pindex, CORE_BURIED[i])`, which the scans and the
        // rotation flag read here from the parameters' heights.
        let deployments = HeightDeployments {
            params: &self.params,
        };
        for (bit, dep) in CORE_BURIED.iter().enumerate() {
            let golden = state.bur >> bit & 1 == 1;
            if golden != deployments.active_after(Some(BlockId(height)), Deployment::Buried(*dep)) {
                self.report.bur_mismatches += 1;
                self.report.mismatch(format!(
                    "C at {height}: bur bit {bit} ({dep:?}) is {golden}, the parameters disagree"
                ));
            }
        }

        let writes = state.qc.unwrap_or_default();
        if writes.is_empty() {
            return Ok(());
        }
        let complete = CompleteChain::<StoreError>::new(&self.chain);
        let llmq = LlmqCtx::new(
            &self.params,
            &complete,
            &deployments,
            bls_ctx_for_block(&self.params, height),
        )
        .map_err(|e| format!("{e:?}"))?;
        let mut records = Vec::with_capacity(writes.len());
        for (llmq_type, quorum_hash, _serialize_hash) in &writes {
            let (t, params) = llmq_params(&self.params, *llmq_type)?;
            let quorum_hash = hash_from_display(quorum_hash)?;
            let base_height =
                *self.heights.get(&BlockHash::from(quorum_hash)).ok_or_else(|| {
                    format!("C at {height}: quorum hash {} not in the chain", display(&quorum_hash))
                })?;
            // `ProcessCommitment`'s `bad-qc-block` check: the quorum hash is the block at the
            // mining block's cycle start plus `quorumIndex`, so the golden's base height fixes it.
            // Core: llmq/blockprocessor.cpp:629-642 (v24.0.0-rc.3) GetQuorumBlockHash, :337-366
            let interval = i64::from(params.dkg_interval);
            let index = i64::from(base_height) - (i64::from(height) - i64::from(height) % interval);
            let index = i16::try_from(index)
                .ok()
                .filter(|i| *i >= 0 && (params.use_rotation || *i == 0))
                .ok_or_else(|| {
                    format!("C at {height}: base {base_height} gives quorum index {index}")
                })?;
            // A placeholder commitment: the golden has no commitment bytes. The replay reads only
            // its type, quorum hash and quorum index (as `listextended` does); the version carries
            // the index for the `useRotation` types (`GetVersion`, llmq/commitment.h:85-92).
            let version = if params.use_rotation {
                4
            } else {
                3
            };
            let qc = null_commitment(t, params, QuorumHash::from(quorum_hash), version, index);
            let record = mined_record(&llmq, params, &qc, BlockId(base_height), base_height)
                .map_err(|e| format!("C at {height}: {e:?}"))?;
            let counts = self.report.per_type.entry(t.to_string()).or_default();
            counts.indexed += 1;
            counts.rotated += u64::from(record.rotation_index.is_some());
            records.push(record);
        }
        self.store
            .apply(&EvoDelta {
                block: hash,
                height,
                dml: None,
                mined_commitments: records,
                credit_data: None,
                credit_pool: None,
                mnhf_signals: None,
                erased_mined: Vec::new(),
                evo_tip: None,
            })
            .map_err(|e| format!("C at {height}: {e}"))
    }

    /// What `quorum listextended h` returns, in the compared fields: per type enabled at the
    /// block at `h` (its own, not its parent's), in configured order, `ScanQuorums(type, block,
    /// signingActiveQuorumCount)`.
    // Core: rpc/quorums.cpp:157-185 (v24.0.0-rc.3) quorum_list_extended;
    // llmq/options.cpp:112 GetEnabledQuorumTypes; llmq/quorumsman.cpp:170 ScanQuorums
    fn list_extended(&self, height: u32) -> Result<Vec<(String, Vec<Row>)>, String> {
        let deployments = HeightDeployments {
            params: &self.params,
        };
        let complete = CompleteChain::<StoreError>::new(&self.chain);
        let llmq = LlmqCtx::new(
            &self.params,
            &complete,
            &deployments,
            bls_ctx_for_block(&self.params, height),
        )
        .map_err(|e| format!("{e:?}"))?;
        let view = self.store.view(&self.chain);
        let at = BlockId(height);
        let mut quorums = Quorums {
            view,
            tip: BlockId(self.tip()),
        };
        let mut out = Vec::new();
        for (llmq_type, params) in enabled_quorum_params(&llmq, at).map_err(|e| format!("{e:?}"))? {
            let count = usize::from(params.signing_active_quorum_count);
            let scanned = match scan_store_block(&llmq, llmq_type, at, count)
                .map_err(|e| format!("{e:?}"))?
            {
                None => Vec::new(),
                // An empty scan cache: a hit returns a prefix of the same scan on one linear chain
                // (`keepOldConnections >= signingActiveQuorumCount` for every testnet type, so a
                // continuation finds nothing the miss did not).
                Some(store) => {
                    scan_quorums_from(
                        &llmq,
                        &view,
                        llmq_type,
                        store,
                        count,
                        ScanCache::Miss,
                        &mut quorums,
                    )
                    .map_err(|e| format!("{e:?}"))?
                    .result
                }
            };
            let rows = scanned
                .iter()
                .map(|q| Row {
                    quorum_hash: display(q.quorum_hash.as_bytes()),
                    quorum_index: params.use_rotation.then_some(i64::from(q.quorum_index)),
                    creation_height: i64::from(q.base.0),
                    mined_block_hash: display(q.mined_block.as_bytes()),
                })
                .collect();
            out.push((llmq_type.to_string(), rows));
        }
        Ok(out)
    }

    /// Core's entries of one type: `[{hash: {fields}}, ...]`, newest first.
    fn core_rows(entries: &Value) -> Result<Vec<Row>, String> {
        let entries = entries.as_array().ok_or("a type's value is not an array")?;
        entries
            .iter()
            .map(|entry| {
                let obj =
                    entry.as_object().filter(|o| o.len() == 1).ok_or("not a one-key object")?;
                let (hash, fields) = obj.iter().next().ok_or("empty")?;
                let int = |k: &str| fields.get(k).and_then(Value::as_i64);
                Ok(Row {
                    quorum_hash: hash.clone(),
                    quorum_index: int("quorumIndex"),
                    creation_height: int("creationHeight").ok_or("no creationHeight")?,
                    mined_block_hash: fields
                        .get("minedBlockHash")
                        .and_then(Value::as_str)
                        .ok_or("no minedBlockHash")?
                        .to_owned(),
                })
            })
            .collect()
    }

    /// A `quorum_listextended` sample at `height` (the chain holds `height`).
    fn compare(&mut self, height: u32, res: &Value) -> Result<(), String> {
        let core = res.as_object().ok_or_else(|| format!("sample at {height}: not an object"))?;
        let ours = self.list_extended(height)?;
        self.report.samples += 1;
        let ours_names: Vec<&str> = ours.iter().map(|(name, _)| name.as_str()).collect();
        let mut core_names: Vec<&str> = core.keys().map(String::as_str).collect();
        core_names.sort_unstable();
        let mut sorted = ours_names.clone();
        sorted.sort_unstable();
        if sorted != core_names {
            self.report.mismatch(format!(
                "sample at {height}: enabled types {ours_names:?}, Core lists {core_names:?}"
            ));
        }
        for (name, rows) in &ours {
            let counts = self.report.per_type.entry(name.clone()).or_default();
            counts.samples += 1;
            let Some(entries) = core.get(name.as_str()) else {
                continue;
            };
            let core_rows =
                Self::core_rows(entries).map_err(|e| format!("sample at {height}, {name}: {e}"))?;
            counts.quorums += core_rows.len() as u64;
            self.report.quorums += core_rows.len() as u64;
            if *rows != core_rows {
                let first = rows.iter().zip(&core_rows).position(|(a, b)| a != b);
                let detail = match first {
                    Some(i) => {
                        format!("first difference at {i}: ours {}, Core {}", rows[i], core_rows[i])
                    }
                    None => "one list is a prefix of the other".to_owned(),
                };
                self.report.mismatch(format!(
                    "sample at {height}, {name}: {} quorums, Core {}; {detail}",
                    rows.len(),
                    core_rows.len()
                ));
            }
        }
        Ok(())
    }

    fn sample(&mut self, rec: Record) -> Result<(), String> {
        if rec.rpc.as_deref() != Some("quorum_listextended") {
            return Ok(());
        }
        let height = rec.h.ok_or("an S record without h")?;
        let args = rec.args.unwrap_or_default();
        if args.len() != 1 || args[0].as_u64() != Some(u64::from(height)) {
            return Err(format!("sample at {height}: args {args:?}"));
        }
        let res = rec.res.ok_or_else(|| format!("sample at {height}: no res"))?;
        let expected = rec.b.as_deref().map(hash_from_display).transpose()?.map(BlockHash::from);
        if self.chain.hashes.len() as u64 > u64::from(height) {
            if expected.is_some() && expected != Some(self.chain.hashes[height as usize]) {
                return Err(format!("sample at {height}: its block is not the chain's"));
            }
            self.compare(height, &res)
        } else {
            self.pending.entry(height).or_default().push(res);
            Ok(())
        }
    }

    fn flush_pending(&mut self) -> Result<(), String> {
        let tip = self.tip();
        while let Some(entry) = self.pending.first_entry() {
            if *entry.key() > tip {
                break;
            }
            let (height, samples) = entry.remove_entry();
            for res in samples {
                self.compare(height, &res)?;
            }
        }
        Ok(())
    }
}

fn open(path: &str) -> Result<Box<dyn Read>, String> {
    if path.ends_with(".zst") {
        let child = Command::new("zstd")
            .args(["-dcq", path])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("zstd: {e}"))?;
        Ok(Box::new(child.stdout.ok_or("zstd's stdout")?))
    } else {
        Ok(Box::new(File::open(path).map_err(|e| format!("{path}: {e}"))?))
    }
}

fn run(path: &str) -> Result<Report, String> {
    let mut replay: Option<Replay> = None;
    let reader = BufReader::with_capacity(1 << 20, open(path)?);
    for (n, line) in reader.lines().enumerate() {
        let line = line.map_err(|e| format!("line {}: {e}", n + 1))?;
        // Other RPC samples are skipped unparsed (quorum_info alone is 216,834 records).
        if line.starts_with(r#"{"k":"S""#) && !line.contains(r#""rpc":"quorum_listextended""#) {
            continue;
        }
        let rec: Record =
            serde_json::from_str(&line).map_err(|e| format!("line {}: {e}", n + 1))?;
        match rec.k.as_str() {
            "H" => {
                let network = match rec.chain.as_deref() {
                    Some("main") => Network::Main,
                    Some("test") => Network::Testnet3,
                    other => return Err(format!("unsupported chain {other:?}")),
                };
                if replay.is_some() {
                    return Err("a second H record".into());
                }
                eprintln!(
                    "golden: chain {:?}, Core {}",
                    rec.chain,
                    rec.ver.as_deref().unwrap_or("?")
                );
                replay = Some(Replay::new(network)?);
            }
            "C" => {
                let replay = replay.as_mut().ok_or("C before H")?;
                replay.connect(rec)?;
                replay.flush_pending()?;
                if replay.report.blocks % 250_000 == 0 {
                    eprintln!(
                        "height {}: {} samples, {} mismatches",
                        replay.tip(),
                        replay.report.samples,
                        replay.report.mismatches
                    );
                }
            }
            "S" => replay.as_mut().ok_or("S before H")?.sample(rec)?,
            other => return Err(format!("line {}: record kind {other} (a disconnect?)", n + 1)),
        }
    }
    let replay = replay.ok_or("no H record")?;
    if let Some(height) = replay.pending.keys().next() {
        return Err(format!("a sample at {height}, above the golden's tip"));
    }
    Ok(replay.report)
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: lq2_listextended <golden.jsonl[.zst]>");
        return ExitCode::from(2);
    };
    let started = Instant::now();
    let report = match run(&path) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    println!(
        "{} blocks; {} quorum_listextended samples compared, {} quorums compared; {} bur \
         mismatches; {} mismatches; {:.1} s",
        report.blocks,
        report.samples,
        report.quorums,
        report.bur_mismatches,
        report.mismatches,
        started.elapsed().as_secs_f64()
    );
    for (name, c) in &report.per_type {
        println!(
            "  {name}: listed in {} samples, {} quorums compared; {} commitments indexed ({} \
             rotated)",
            c.samples, c.quorums, c.indexed, c.rotated
        );
    }
    for m in &report.shown {
        println!("MISMATCH {m}");
    }
    if report.mismatches == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
