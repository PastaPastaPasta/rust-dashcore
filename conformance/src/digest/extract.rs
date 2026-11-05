//! `digest extract`: the StateDigest v1 golden extractor (spec §9).
//!
//! Input: a frozen copy of a recording node, reached over JSON-RPC, and that node's
//! `digest.jsonl`. Output: `golden.jsonl`, an `H` record, one merged `C` record per active-chain
//! block (the last `org:"tip"` `C` of the digest plus the §5.3 golden keys), then the sparse `S`
//! records of §9.3. Every line is written with the `dash-statedigest` encoder, and the digest is
//! read with its parser.
//!
//! # Decisions where the spec is open
//!
//! * **Eras** come from the node's consensus parameters, the buried heights `getdeploymentinfo`
//!   reports for `dip0003` and `v20`, never from the digest under check (§13.5). A rotated quorum
//!   type is one whose `quorum info` carries `previousConsecutiveDKGFailures`.
//! * **Every patch key of `s` is checked** against another source before its `C` record is
//!   written ([`checks`], §13.5), and so is `quorum info`'s `minedBlock` (the block of the `qc`
//!   row). With `--dense`, the extraction fails unless every era-driven `S` record of §9.3 was
//!   written.
//! * **`S.args`** are the RPC arguments after the command words: `protx list registered true <h>`
//!   is `["registered",true,<h>]`, `quorum info <type> <hash>` is `[<type>,"<hash>"]`. The `rpc`
//!   name is the §9.3 table's, so the command is implied. Heights are integers, hashes strings.
//! * **`S.h`/`S.b`** name the block the call is about: the call's height argument (the `to`
//!   height for `listdiff`), the block that mined the row for `quorum_info`, the cycle base
//!   block for `rotationinfo`.
//! * **`rotationinfo`** is called once per distinct cycle base of every mined rotated
//!   commitment: the base is `quorum info`'s `height - quorumIndex`, and the request hash is
//!   that block's hash.
//! * **`assetunlockstatuses`** runs on V20 blocks with more than one transaction whose
//!   `getblock <hash> 2` lists `type 9` transactions; the indexes are sent as decimal strings
//!   (dashd's implementation reads them with `get_str`, whatever its help text says), in
//!   transaction order.
//! * **Window boundaries** for `deploymentinfo` are the heights where `bur` or `dep` differ from
//!   the previous block's, plus the `--boundary` set.
//! * **Order of `S` records**: ascending `h`, then the §9.3 table order, then the order the calls
//!   were made in (`qc` row order). Output order never depends on a map's iteration order.
//! * **Genesis** gets the `muhash`, `txouts`, `amount`, `unspendable` and `bi` keys from the RPC
//!   and `stats: null` (§5.3: `stats` is non-genesis).
//! * **Wall-clock keys are dropped** (§9.5): `protx list`'s `metaInfo` carries
//!   `lastOutboundAttemptElapsed` and `lastOutboundSuccessElapsed`, which are `now - t` and so
//!   differ on every extraction. Every `metaInfo` member whose name ends in `Elapsed` is removed
//!   from `S.res`; the absolute timestamps next to them are kept (they come from the datadir).
//! * **RPC errors** in an in-era call fail the extraction: a golden never records a hole. The one
//!   exception is `rotationinfo`: dashd refuses it for early cycle bases (the snapshots of the
//!   three earlier cycles are not stored yet), and nothing in the digest tells the extractor
//!   which bases those are. A refusal skips that record and is listed in [`Summary::skipped`],
//!   which the CLI prints.
//! * **More than 100 asset unlocks** in a block go out in calls of at most 100 indexes (dashd's
//!   limit), one `S` record per call.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use dash_statedigest::{
    Chain, FileKind, Hash256, Header, Kind, Lines, Origin, Outcome, Record, Sparse, Value, Writer,
};
use serde_json::json;

use super::amount::{duffs_from_amount, int_from_number};
use super::json;
use crate::rpc::Rpc;

/// `getblockstats`' named stats, in `stats` order (§5.3).
const STATS: [&str; 7] =
    ["subsidy", "totalfee", "txs", "ins", "outs", "utxo_increase", "utxo_size_inc"];

/// A node's error reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

/// A node's reply: the `result`, with every number kept as text, or its error.
pub type Reply = Result<Value, RpcError>;

/// The node under extraction. `Err` is a transport or parse failure, `Ok(Err(_))` the node's own
/// error reply.
pub trait NodeRpc {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<Reply>;
}

impl NodeRpc for Rpc {
    fn call(&self, method: &str, params: serde_json::Value) -> Result<Reply> {
        self.call_raw(method, params, parse_reply)
    }
}

/// Reads a JSON-RPC reply body into a [`Reply`].
pub fn parse_reply(body: &[u8]) -> Result<Reply> {
    let reply = json::parse(body)?;
    if let Some(err) = reply.get(b"error").filter(|e| !e.is_null()) {
        let code = err
            .get(b"code")
            .and_then(Value::as_num)
            .map(int_from_number)
            .transpose()?
            .ok_or_else(|| anyhow!("error reply without a code"))?;
        let message = err.get(b"message").and_then(Value::as_bytes).unwrap_or_default();
        return Ok(Err(RpcError {
            code,
            message: String::from_utf8_lossy(message).into_owned(),
        }));
    }
    let result = reply.get(b"result").ok_or_else(|| anyhow!("reply without result"))?;
    Ok(Ok(result.clone()))
}

/// The sparse RPCs of §9.3, in table order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SparseRpc {
    ProtxList,
    ProtxListdiff,
    MnPayments,
    QuorumListextended,
    QuorumInfo,
    Rotationinfo,
    Deploymentinfo,
    Creditpool,
    Assetunlockstatuses,
    CanonicalMnlist,
}

impl SparseRpc {
    /// Every RPC, in table order.
    pub const ALL: [SparseRpc; 10] = [
        SparseRpc::ProtxList,
        SparseRpc::ProtxListdiff,
        SparseRpc::MnPayments,
        SparseRpc::QuorumListextended,
        SparseRpc::QuorumInfo,
        SparseRpc::Rotationinfo,
        SparseRpc::Deploymentinfo,
        SparseRpc::Creditpool,
        SparseRpc::Assetunlockstatuses,
        SparseRpc::CanonicalMnlist,
    ];

    /// The `S.rpc` text.
    pub const fn name(self) -> &'static str {
        match self {
            SparseRpc::ProtxList => "protx_list",
            SparseRpc::ProtxListdiff => "protx_listdiff",
            SparseRpc::MnPayments => "mn_payments",
            SparseRpc::QuorumListextended => "quorum_listextended",
            SparseRpc::QuorumInfo => "quorum_info",
            SparseRpc::Rotationinfo => "rotationinfo",
            SparseRpc::Deploymentinfo => "deploymentinfo",
            SparseRpc::Creditpool => "creditpool",
            SparseRpc::Assetunlockstatuses => "assetunlockstatuses",
            SparseRpc::CanonicalMnlist => "canonical_mnlist",
        }
    }

    /// The JSON-RPC method and its subcommand, if the method is a command family.
    const fn command(self) -> (&'static str, Option<&'static str>) {
        match self {
            SparseRpc::ProtxList => ("protx", Some("list")),
            SparseRpc::ProtxListdiff => ("protx", Some("listdiff")),
            SparseRpc::MnPayments => ("masternode", Some("payments")),
            SparseRpc::QuorumListextended => ("quorum", Some("listextended")),
            SparseRpc::QuorumInfo => ("quorum", Some("info")),
            SparseRpc::Rotationinfo => ("quorum", Some("rotationinfo")),
            SparseRpc::Deploymentinfo => ("getdeploymentinfo", None),
            SparseRpc::Creditpool => ("getcreditpoolinfo", None),
            SparseRpc::Assetunlockstatuses => ("getassetunlockstatuses", None),
            SparseRpc::CanonicalMnlist => ("getcanonicalmnlist", None),
        }
    }

    /// The JSON-RPC call for `args`.
    fn request(self, args: &[Value]) -> Result<(&'static str, serde_json::Value)> {
        let (method, sub) = self.command();
        let mut params: Vec<serde_json::Value> = sub.map(|s| json!(s)).into_iter().collect();
        for a in args {
            params.push(to_json(a)?);
        }
        Ok((method, serde_json::Value::Array(params)))
    }
}

/// An argument value as JSON (arguments are integers, strings, booleans and arrays of them).
fn to_json(v: &Value) -> Result<serde_json::Value> {
    Ok(match v {
        Value::Bool(b) => json!(b),
        Value::Int(n) => json!(n),
        Value::Str(s) => json!(std::str::from_utf8(s).context("non-UTF-8 RPC argument")?),
        Value::Arr(items) => {
            serde_json::Value::Array(items.iter().map(to_json).collect::<Result<_>>()?)
        }
        other => bail!("unsupported RPC argument {other:?}"),
    })
}

/// Extractor settings.
#[derive(Clone, Debug)]
pub struct Options {
    /// Regtest cadence (§9.3's last column): the table's "every block" rows run at every block.
    pub dense: bool,
    /// Segment heights for the "plus boundaries" rows, added to the chain's own set
    /// ([`Options::default_boundaries`]).
    pub boundaries: BTreeSet<i64>,
    /// How long to wait for `coinstatsindex` to sync.
    pub wait: Duration,
    /// Pause between readiness polls.
    pub poll: Duration,
    /// The frozen copy's `blocks` directory, whose `rev*.dat` files check `undo` (§13.5). `None`
    /// leaves `undo` unchecked.
    pub blocks_dir: Option<PathBuf>,
    /// Digest runs (`H.run`) whose node ran with other BIP9 parameters (`-vbparams`) than the
    /// frozen copy, which restarts with the last start's: their records' BIP9 values cannot be
    /// checked against it (§13.5). Each must be a run of the digest.
    pub other_params_runs: BTreeSet<Vec<u8>>,
}

/// The mainnet segment heights of A1 §4.6 (DGW exactness, treasury, superblocks, DIP3, DIP3
/// enforcement, DIP8, BRR, DIP24, V19, V20, MN_RR, WITHDRAWALS). The document gives no testnet
/// equivalents and regtest has none; pass `--boundary` for those.
pub const MAINNET_BOUNDARIES: [i64; 12] = [
    68589, 328008, 614820, 1028159, 1047199, 1088639, 1374911, 1737791, 1899071, 1987775, 2128895,
    2201471,
];

impl Options {
    /// The boundary set a chain starts from: A1's list on mainnet, nothing elsewhere.
    pub fn default_boundaries(chain: Chain) -> BTreeSet<i64> {
        if chain == Chain::Main {
            MAINNET_BOUNDARIES.into_iter().collect()
        } else {
            BTreeSet::new()
        }
    }
}

/// What the extraction wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub chain: Chain,
    pub tip: i64,
    /// The `H.run` value: the frozen copy's best block hash.
    pub run: String,
    /// Merged `C` records written.
    pub blocks: u64,
    /// `S` records written per `rpc` name.
    pub sparse: BTreeMap<&'static str, u64>,
    /// Calls the node refused and the extraction skipped (only `rotationinfo`, see the module
    /// doc): the call, the error code and message. Empty on a clean run.
    pub skipped: Vec<String>,
    /// The heights of the merged `C` records whose BIP9 values were left unchecked: every tip `C`
    /// of the block came from one of [`Options::other_params_runs`]. [`check_bip9`] checks them
    /// on a copy started with those runs' parameters.
    pub bip9_unchecked: Vec<i64>,
    /// The blocks whose `mnl` agreed with `getcanonicalmnlist` only after the registration
    /// counter adjustment (§13.5).
    pub mnl_counter_adjusted: u64,
}

/// What the cadence rules read from a block's digest record.
#[derive(Clone, Debug)]
struct BlockInfo {
    h: i64,
    hash: Hash256,
    /// DIP0003 era (from the node's parameters).
    dip3: bool,
    /// V20 era (from the node's parameters).
    v20: bool,
    /// This block's mined commitments: `(type, quorumHash)`.
    qc: Vec<(i64, Hash256)>,
    /// The asset-unlock indexes of its `getblock <hash> 2`, in transaction order, as the node
    /// printed them. Left empty for the caller to set.
    unlocks: Vec<String>,
    /// `bur` or `dep` differ from the previous block's.
    transition: bool,
}

/// Whether the cadence-driven RPC `rpc` is due at `info` (§9.3). `QuorumInfo`, `Rotationinfo` and
/// `Assetunlockstatuses` are driven by the block's contents, not by a cadence, and are never due
/// here.
fn due(rpc: SparseRpc, info: &BlockInfo, opts: &Options) -> bool {
    let h = info.h;
    let every_576 = h % 576 == 0;
    let boundary = opts.boundaries.contains(&h);
    let near_boundary = opts.boundaries.range(h - 3..=h + 3).next().is_some();
    match rpc {
        SparseRpc::ProtxList => info.dip3 && (opts.dense || every_576 || boundary),
        // `protx listdiff` refuses base block 0.
        SparseRpc::ProtxListdiff => {
            info.dip3 && h >= 2 && (opts.dense || every_576 || near_boundary)
        }
        SparseRpc::MnPayments => info.dip3 && (opts.dense || every_576),
        SparseRpc::QuorumListextended => info.dip3 && (opts.dense || h % 24 == 0),
        SparseRpc::Deploymentinfo => opts.dense || every_576 || boundary || info.transition,
        SparseRpc::Creditpool => info.v20 && (opts.dense || every_576),
        SparseRpc::CanonicalMnlist => {
            info.dip3
                && if opts.dense {
                    h % 24 == 0
                } else {
                    every_576 || boundary
                }
        }
        SparseRpc::QuorumInfo | SparseRpc::Rotationinfo | SparseRpc::Assetunlockstatuses => false,
    }
}

fn get<'a>(v: &'a Value, key: &str) -> Result<&'a Value> {
    v.get(key.as_bytes()).ok_or_else(|| anyhow!("reply lacks {key:?}"))
}

fn num<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    get(v, key)?.as_num().ok_or_else(|| anyhow!("{key:?} is not a number"))
}

fn int(v: &Value, key: &str) -> Result<i64> {
    int_from_number(num(v, key)?).with_context(|| format!("field {key:?}"))
}

fn amount(v: &Value, key: &str) -> Result<i64> {
    duffs_from_amount(num(v, key)?).with_context(|| format!("field {key:?}"))
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    get(v, key)?.as_str().ok_or_else(|| anyhow!("{key:?} is not a string"))
}

/// The node, with errors that name the call.
struct Client<'a> {
    node: &'a dyn NodeRpc,
}

impl Client<'_> {
    fn raw(&self, method: &str, params: serde_json::Value) -> Result<Reply> {
        self.node.call(method, params.clone()).with_context(|| format!("{method} {params}"))
    }

    fn call(&self, method: &str, params: serde_json::Value) -> Result<Value> {
        match self.raw(method, params.clone())? {
            Ok(v) => Ok(v),
            Err(e) => bail!("{method} {params} failed: code {}: {}", e.code, e.message),
        }
    }

    fn block_hash(&self, h: i64) -> Result<Hash256> {
        let v = self.call("getblockhash", json!([h]))?;
        v.as_hash().ok_or_else(|| anyhow!("getblockhash {h}: not a block hash"))
    }

    fn best_hash(&self) -> Result<Hash256> {
        let v = self.call("getbestblockhash", json!([]))?;
        v.as_hash().ok_or_else(|| anyhow!("getbestblockhash: not a block hash"))
    }

    /// Waits until `coinstatsindex` reports synced at the tip (§9.1).
    fn wait_for_index(&self, wait: Duration, poll: Duration) -> Result<()> {
        let deadline = Instant::now() + wait;
        loop {
            let state = match self.raw("getindexinfo", json!([])) {
                Ok(Ok(info)) => {
                    let Some(ix) = info.get(b"coinstatsindex") else {
                        bail!("the node has no coinstatsindex: start it with -coinstatsindex=1");
                    };
                    let synced = ix.get(b"synced").and_then(Value::as_bool) == Some(true);
                    let at = ix.get(b"best_block_height").and_then(Value::as_num);
                    let tip = self.tip_height()?;
                    if synced && at.map(int_from_number) == Some(Ok(tip)) {
                        return Ok(());
                    }
                    format!("synced={synced}, index height {at:?}, tip {tip}")
                }
                // -28: still loading.
                Ok(Err(e)) if e.code == -28 => e.message,
                Ok(Err(e)) => bail!("getindexinfo failed: code {}: {}", e.code, e.message),
                // A transport failure: still starting.
                Err(e) => format!("{e:#}"),
            };
            if Instant::now() >= deadline {
                bail!("coinstatsindex did not reach the tip in time ({state})");
            }
            std::thread::sleep(poll);
        }
    }

    fn tip_height(&self) -> Result<i64> {
        int(&self.call("getblockchaininfo", json!([]))?, "blocks")
    }
}

/// One merged-`C` source: the last tip connect of a block, with where it was.
struct Connect {
    line: usize,
    rec: Outcome,
    /// Some tip `C` of the block came from a run with the frozen copy's parameters.
    same_params: bool,
}

/// Reads `digest.jsonl`: the last `org:"tip"` `C` per block hash (§9.2 step 1).
fn load_digest(
    bytes: &[u8],
    chain: Chain,
    other_params_runs: &BTreeSet<Vec<u8>>,
) -> Result<HashMap<Hash256, Connect>> {
    if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        bail!("the digest is zstd-compressed: decompress it first (zstd -d)");
    }
    let mut connects: HashMap<Hash256, Connect> = HashMap::new();
    let mut runs: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut same_params = true;
    for (line, parsed) in Lines::new(bytes) {
        let rec = parsed.with_context(|| format!("digest line {line}"))?;
        match rec {
            Record::Header(h) => {
                ensure!(h.file == FileKind::Digest, "digest line {line}: not a digest file's H");
                ensure!(
                    h.chain == chain,
                    "digest line {line}: chain {} but the node is on {}",
                    h.chain.as_str(),
                    chain.as_str()
                );
                same_params = !other_params_runs.contains(&h.run);
                runs.insert(h.run);
            }
            Record::Outcome(o) if o.kind() == Kind::Connect && o.id.org == Origin::Tip => {
                if let Some(prev) = connects.get(&o.id.b)
                    && prev.rec.s() != o.s()
                {
                    let key = o
                        .s()
                        .specs()
                        .iter()
                        .find(|f| prev.rec.s().get(f.key) != o.s().get(f.key))
                        .map_or("?", |f| f.key);
                    bail!(
                        "digest lines {} and {}: C records of block {} at height {} disagree \
                         on s ({key:?} differs)",
                        prev.line,
                        line,
                        o.id.b,
                        o.id.h
                    );
                }
                let same_params =
                    same_params || connects.get(&o.id.b).is_some_and(|p| p.same_params);
                connects.insert(
                    o.id.b,
                    Connect {
                        line,
                        rec: o,
                        same_params,
                    },
                );
            }
            _ => {}
        }
    }
    ensure!(!runs.is_empty(), "the digest has no H record");
    if let Some(r) = other_params_runs.difference(&runs).next() {
        bail!(
            "run {} has other parameters, but the digest has no such run",
            String::from_utf8_lossy(r)
        );
    }
    Ok(connects)
}

/// Reads what the cadence rules need from a digest `C`; fails if a required key is absent.
/// `transition` is left false for the caller to set.
fn block_info(c: &Outcome, eras: checks::Eras) -> Result<BlockInfo> {
    let s = c.s();
    let need = |key: &str| s.get(key).ok_or_else(|| anyhow!("the C record lacks s.{key}"));
    let qc = match need("qc")? {
        Value::Null => Vec::new(),
        rows => rows
            .as_arr()
            .ok_or_else(|| anyhow!("s.qc is neither null nor an array"))?
            .iter()
            .map(|row| match row.as_arr() {
                Some([t, q, _]) => Ok((
                    t.as_int().ok_or_else(|| anyhow!("qc type"))?,
                    q.as_hash().ok_or_else(|| anyhow!("qc hash"))?,
                )),
                _ => Err(anyhow!("malformed qc row")),
            })
            .collect::<Result<_>>()?,
    };
    need("bur")?;
    need("dep")?;
    Ok(BlockInfo {
        h: c.id.h,
        hash: c.id.b,
        dip3: c.id.h >= eras.dip3,
        v20: c.id.h >= eras.v20,
        qc,
        unlocks: Vec::new(),
        transition: false,
    })
}

/// §5.3's golden values for one block.
struct Golden {
    muhash: String,
    txouts: i64,
    amount: i64,
    unspendable: i64,
    bi: [i64; 7],
    stats: Option<[i64; 7]>,
}

fn golden(cl: &Client<'_>, h: i64, hash: &Hash256) -> Result<Golden> {
    let info = cl.call("gettxoutsetinfo", json!(["muhash", h, true]))?;
    ensure!(int(&info, "height")? == h, "gettxoutsetinfo answered for another height");
    ensure!(
        get(&info, "bestblock")?.as_hash().as_ref() == Some(hash),
        "gettxoutsetinfo answered for another block"
    );
    let muhash = text(&info, "muhash")?;
    ensure!(
        muhash.len() == 64 && muhash.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')),
        "muhash is not 64 lowercase hex digits: {muhash:?}"
    );
    let bi =
        get(&info, "block_info").context("no block_info: is the coinstatsindex on and synced?")?;
    let unspendables = get(bi, "unspendables")?;
    let golden = Golden {
        muhash: muhash.to_owned(),
        txouts: int(&info, "txouts")?,
        amount: amount(&info, "total_amount")?,
        unspendable: amount(&info, "total_unspendable_amount")?,
        bi: [
            amount(bi, "prevout_spent")?,
            amount(bi, "coinbase")?,
            amount(bi, "new_outputs_ex_coinbase")?,
            amount(bi, "unspendable")?,
            amount(unspendables, "genesis_block")?,
            amount(unspendables, "scripts")?,
            amount(unspendables, "unclaimed_rewards")?,
        ],
        stats: None,
    };
    if h == 0 {
        return Ok(golden);
    }
    let stats = cl.call("getblockstats", json!([h, STATS]))?;
    let mut values = [0i64; 7];
    for (slot, key) in values.iter_mut().zip(STATS) {
        *slot = int(&stats, key)?;
    }
    Ok(Golden {
        stats: Some(values),
        ..golden
    })
}

fn ints(values: &[i64]) -> Value {
    Value::Arr(values.iter().copied().map(Value::Int).collect())
}

/// Appends §5.3's keys to a merged `C`'s `s`.
fn set_golden(rec: &mut Outcome, g: &Golden) -> Result<()> {
    let mut s = rec.s_mut();
    let items = [
        ("muhash", Value::string(&g.muhash)),
        ("txouts", Value::Int(g.txouts)),
        ("amount", Value::Int(g.amount)),
        ("unspendable", Value::Int(g.unspendable)),
        ("bi", ints(&g.bi)),
        ("stats", g.stats.as_ref().map_or(Value::Null, |v| ints(v))),
    ];
    for (key, value) in items {
        let replaced = s.set(key, value).map_err(|e| anyhow!("s.{key}: {e}"))?;
        ensure!(replaced.is_none(), "the digest's C record already has the golden key s.{key}");
    }
    Ok(())
}

/// A sparse call to make.
struct Call {
    rpc: SparseRpc,
    /// The block the record is about.
    h: i64,
    hash: Hash256,
    args: Vec<Value>,
}

/// Removes the wall-clock-derived members (`metaInfo.*Elapsed`) from an `S` payload, so that two
/// extractions of one frozen copy agree (§9.5).
fn strip_wall_clock(v: &mut Value) {
    match v {
        Value::Arr(items) => items.iter_mut().for_each(strip_wall_clock),
        Value::Obj(members) => {
            for (key, child) in members.iter_mut() {
                if key == b"metaInfo"
                    && let Value::Obj(meta) = child
                {
                    meta.retain(|(k, _)| !k.ends_with(b"Elapsed"));
                }
                strip_wall_clock(child);
            }
        }
        _ => {}
    }
}

/// Puts `protx listdiff`'s `updatedMNs` (`[{proTxHash: diff}]`) in proTxHash order. Core fills
/// the array from `CDeterministicMNListDiff::updatedMNs`, a salted `std::unordered_map`
/// (`evo/deterministicmns.h` v24:635), so its order changes with every process (§9.5).
fn order_updated_mns(rpc: SparseRpc, v: &mut Value) {
    if rpc != SparseRpc::ProtxListdiff {
        return;
    }
    let Value::Obj(members) = v else {
        return;
    };
    for (key, child) in members.iter_mut() {
        if key == b"updatedMNs"
            && let Value::Arr(items) = child
        {
            items.sort_by(|a, b| first_key(a).cmp(first_key(b)));
        }
    }
}

fn first_key(v: &Value) -> &[u8] {
    match v {
        Value::Obj(members) => members.first().map_or(&[], |(k, _)| k.as_slice()),
        _ => &[],
    }
}

/// One sparse record with its sort key.
struct Pending {
    key: (i64, SparseRpc, usize),
    line: Vec<u8>,
}

struct Collector<'a> {
    cl: &'a Client<'a>,
    pending: Vec<Pending>,
    seq: usize,
    skipped: Vec<String>,
}

impl Collector<'_> {
    /// Makes `call` and queues its `S` record. Returns the payload. An error reply fails the
    /// extraction.
    fn run(&mut self, call: Call) -> Result<Value> {
        self.run_or_skip(call, false)?.ok_or_else(|| anyhow!("unreachable: call not skipped"))
    }

    /// Like [`Collector::run`], but with `tolerate`, a refusal (`RPC_INVALID_REQUEST`, the code
    /// `quorum rotationinfo` gives when `BuildQuorumRotationInfo` fails) skips the record and is
    /// listed in [`Summary::skipped`]; any other error still fails.
    fn run_or_skip(&mut self, call: Call, tolerate: bool) -> Result<Option<Value>> {
        let (method, params) = call.rpc.request(&call.args)?;
        let what = format!("{} at height {} ({})", call.rpc.name(), call.h, call.hash);
        let mut res = match self.cl.raw(method, params.clone()).with_context(|| what.clone())? {
            Ok(res) => res,
            Err(e) if tolerate && e.code == RPC_INVALID_REQUEST => {
                self.skipped.push(format!("{what}: code {}: {}", e.code, e.message));
                return Ok(None);
            }
            Err(e) => bail!("{what}: {method} {params} failed: code {}: {}", e.code, e.message),
        };
        strip_wall_clock(&mut res);
        order_updated_mns(call.rpc, &mut res);
        let sparse = Sparse::new(call.h, call.hash, call.rpc.name(), call.args, res.clone())
            .map_err(|e| anyhow!("{} at height {}: {e}", call.rpc.name(), call.h))?;
        self.seq += 1;
        self.pending.push(Pending {
            key: (call.h, call.rpc, self.seq),
            line: Record::Sparse(sparse).to_line(),
        });
        Ok(Some(res))
    }
}

/// With `--dense`, the era-driven rows of §9.3 must cover every block of their era: counted here
/// from the eras and the `qc` rows, not from the cadence rules that made the calls.
fn check_dense_coverage(infos: &[BlockInfo], sparse: &BTreeMap<&'static str, u64>) -> Result<()> {
    let count = |f: &dyn Fn(&BlockInfo) -> bool| infos.iter().filter(|i| f(i)).count() as u64;
    let want = [
        (SparseRpc::ProtxList, count(&|i| i.dip3)),
        (SparseRpc::ProtxListdiff, count(&|i| i.dip3 && i.h >= 2)),
        (SparseRpc::MnPayments, count(&|i| i.dip3)),
        (SparseRpc::QuorumListextended, count(&|i| i.dip3)),
        (SparseRpc::QuorumInfo, infos.iter().map(|i| i.qc.len() as u64).sum()),
        (SparseRpc::Deploymentinfo, infos.len() as u64),
        (SparseRpc::Creditpool, count(&|i| i.v20)),
        (SparseRpc::CanonicalMnlist, count(&|i| i.dip3 && i.h % 24 == 0)),
    ];
    for (rpc, n) in want {
        let got = sparse.get(rpc.name()).copied().unwrap_or(0);
        ensure!(got == n, "{} S records: {got}, but the eras call for {n}", rpc.name());
    }
    Ok(())
}

/// Core's `RPC_INVALID_REQUEST` (`rpc/protocol.h`).
const RPC_INVALID_REQUEST: i64 = -32600;

/// Extracts the golden file for the frozen copy behind `node` from `digest`, writing it to `out`.
pub fn extract(
    node: &dyn NodeRpc,
    digest: &[u8],
    opts: &Options,
    out: &mut dyn Write,
) -> Result<Summary> {
    let cl = Client {
        node,
    };
    cl.wait_for_index(opts.wait, opts.poll)?;

    let chain_info = cl.call("getblockchaininfo", json!([]))?;
    let chain = Chain::from_bytes(text(&chain_info, "chain")?.as_bytes())
        .ok_or_else(|| anyhow!("unknown chain name"))?;
    let tip = int(&chain_info, "blocks")?;
    let run = cl.best_hash()?;
    ensure!(
        get(&chain_info, "bestblockhash")?.as_hash() == Some(run),
        "the node's tip moved while it was being read"
    );
    let ver = text(&cl.call("getnetworkinfo", json!([]))?, "buildversion")?.to_owned();

    let mut opts = opts.clone();
    opts.boundaries.extend(Options::default_boundaries(chain));
    let opts = &opts;

    let mut connects = load_digest(digest, chain, &opts.other_params_runs)?;
    let mut checker = checks::Checker::new(&cl, opts.blocks_dir.as_deref())?;

    let mut write = |bytes: &[u8]| out.write_all(bytes).context("writing the golden file");
    write(
        &Record::Header(Header {
            file: FileKind::Golden,
            src: Writer::Extract,
            ver: ver.into_bytes(),
            chain,
            run: run.to_hex().into_bytes(),
        })
        .to_line(),
    )?;

    // Merged C records.
    let mut infos: Vec<BlockInfo> = Vec::with_capacity(usize::try_from(tip + 1).unwrap_or(0));
    // `(bur, dep)` of the previous block, for the window-boundary rule.
    let mut prev_state: Option<(Option<Value>, Option<Value>)> = None;
    let mut bip9_unchecked = Vec::new();
    // A node that started from a copied datadir records from its first own connect: the digest
    // must cover a contiguous suffix of the active chain, which the golden then covers (§9.2).
    for h in 0..=tip {
        let hash = cl.block_hash(h)?;
        let Some(Connect {
            line,
            rec: mut c,
            same_params,
        }) = connects.remove(&hash)
        else {
            ensure!(
                infos.is_empty(),
                "the digest has no tip C record for block {h} {hash} of the active chain"
            );
            checker.skip(h, &hash)?;
            continue;
        };
        ensure!(c.id.h == h, "digest line {line}: block {hash} is at height {h}, not {}", c.id.h);
        if let Some(p) = &infos.last() {
            let want = Value::hash(&p.hash);
            ensure!(
                c.s().get("prev") == Some(&want),
                "digest line {line}: prev of block {h} is not the active chain's block {}",
                h - 1
            );
        }
        if !same_params {
            bip9_unchecked.push(h);
        }
        let unlocks = checker
            .check(&c, h, &hash, same_params)
            .with_context(|| format!("digest line {line}, block {h}"))?;
        let mut info =
            block_info(&c, checker.eras).with_context(|| format!("digest line {line}"))?;
        info.unlocks = unlocks;
        let state = (c.s().get("bur").cloned(), c.s().get("dep").cloned());
        info.transition = prev_state.as_ref().is_some_and(|p| *p != state);
        prev_state = Some(state);
        set_golden(&mut c, &golden(&cl, h, &hash)?)?;
        write(&Record::Outcome(c).to_line())?;
        infos.push(info);
        if h % 50_000 == 49_999 {
            eprintln!("golden C records: {} of {}", h + 1, tip + 1);
        }
    }

    ensure!(!infos.is_empty(), "the digest has no tip C record of the active chain");

    // Sparse records.
    let mut col = Collector {
        cl: &cl,
        pending: Vec::new(),
        seq: 0,
        skipped: Vec::new(),
    };
    let mut cycle_bases: BTreeSet<i64> = BTreeSet::new();
    for info in &infos {
        let at = |rpc: SparseRpc, args: Vec<Value>| Call {
            rpc,
            h: info.h,
            hash: info.hash,
            args,
        };
        let hash_arg = || Value::hash(&info.hash);
        for rpc in SparseRpc::ALL {
            match rpc {
                SparseRpc::QuorumInfo => {
                    for (ty, quorum) in &info.qc {
                        let res = col.run(at(rpc, vec![Value::Int(*ty), Value::hash(quorum)]))?;
                        ensure!(
                            get(&res, "minedBlock")?.as_hash() == Some(info.hash),
                            "quorum {quorum} of s.qc at height {} was mined in another block",
                            info.h
                        );
                        // Only rotated types report this (`BuildQuorumInfo`).
                        if res.get(b"previousConsecutiveDKGFailures").is_some() {
                            let height = int(&res, "height")?;
                            let base = height - int(&res, "quorumIndex")?;
                            ensure!(
                                (0..=tip).contains(&base),
                                "quorum {quorum} has cycle base height {base} outside the chain"
                            );
                            cycle_bases.insert(base);
                        }
                    }
                }
                SparseRpc::Rotationinfo => {} // after the walk, per cycle base
                SparseRpc::Assetunlockstatuses => {
                    // dashd refuses more than 100 indexes per call: one record per chunk.
                    for chunk in info.unlocks.chunks(100) {
                        let list = chunk.iter().map(Value::string).collect();
                        col.run(at(rpc, vec![Value::Arr(list), Value::Int(info.h)]))?;
                    }
                }
                _ if !due(rpc, info, opts) => {}
                SparseRpc::ProtxList => {
                    col.run(at(
                        rpc,
                        vec![Value::string("registered"), Value::Bool(true), Value::Int(info.h)],
                    ))?;
                }
                SparseRpc::ProtxListdiff => {
                    col.run(at(rpc, vec![Value::Int(info.h - 1), Value::Int(info.h)]))?;
                }
                SparseRpc::Creditpool | SparseRpc::QuorumListextended => {
                    col.run(at(rpc, vec![Value::Int(info.h)]))?;
                }
                SparseRpc::MnPayments | SparseRpc::Deploymentinfo | SparseRpc::CanonicalMnlist => {
                    col.run(at(rpc, vec![hash_arg()]))?;
                }
            }
        }
    }
    for base in cycle_bases {
        let hash = cl.block_hash(base)?;
        let call = Call {
            rpc: SparseRpc::Rotationinfo,
            h: base,
            hash,
            args: vec![Value::hash(&hash)],
        };
        col.run_or_skip(call, true)?;
    }

    // The frozen copy must not have moved under us.
    ensure!(
        cl.best_hash()? == run,
        "the node's tip changed during the extraction: the copy was not frozen"
    );

    let mut pending = col.pending;
    pending.sort_by_key(|p| p.key);
    let mut sparse: BTreeMap<&'static str, u64> = BTreeMap::new();
    for p in &pending {
        *sparse.entry(p.key.1.name()).or_default() += 1;
    }
    if opts.dense {
        check_dense_coverage(&infos, &sparse)?;
    }
    for p in &pending {
        write(&p.line)?;
    }
    Ok(Summary {
        chain,
        tip,
        run: run.to_hex(),
        blocks: u64::try_from(infos.len())?,
        sparse,
        skipped: col.skipped,
        bip9_unchecked,
        mnl_counter_adjusted: checker.mnl_counter_adjusted,
    })
}

/// The runs (`H.run`) of a digest.
fn digest_runs(digest: &[u8]) -> Result<BTreeSet<Vec<u8>>> {
    let mut runs = BTreeSet::new();
    for (line, parsed) in Lines::new(digest) {
        if let Record::Header(h) = parsed.with_context(|| format!("digest line {line}"))? {
            runs.insert(h.run);
        }
    }
    Ok(runs)
}

/// What [`check_bip9`] checked.
#[derive(Debug)]
pub struct Bip9Check {
    /// The copy's best block hash, which must be the extraction's `H.run`.
    pub tip: Hash256,
    /// The heights whose BIP9 values were checked.
    pub checked: Vec<i64>,
}

/// Checks the BIP9 values (`dep`, its `since`, and the EHF heights the node reports, §13.5) of
/// every active-chain block that has a tip `C` from one of `runs`, on a copy started with those
/// runs' parameters: the counterpart of [`Options::other_params_runs`].
pub fn check_bip9(
    node: &dyn NodeRpc,
    digest: &[u8],
    runs: &BTreeSet<Vec<u8>>,
) -> Result<Bip9Check> {
    let cl = Client {
        node,
    };
    let chain_info = cl.call("getblockchaininfo", json!([]))?;
    let chain = Chain::from_bytes(text(&chain_info, "chain")?.as_bytes())
        .ok_or_else(|| anyhow!("unknown chain name"))?;
    let tip = int(&chain_info, "blocks")?;
    let all = digest_runs(digest)?;
    if let Some(r) = runs.difference(&all).next() {
        bail!("the digest has no run {}", String::from_utf8_lossy(r));
    }
    // `same_params` then marks the blocks with a tip C from one of `runs`.
    let others: BTreeSet<Vec<u8>> = all.difference(runs).cloned().collect();
    let connects = load_digest(digest, chain, &others)?;
    let mut checker = checks::Checker::new(&cl, None)?;
    let mut checked = Vec::new();
    for h in 0..=tip {
        let hash = cl.block_hash(h)?;
        if let Some(c) = connects.get(&hash).filter(|c| c.same_params) {
            checker
                .check_bip9(&c.rec, h, &hash)
                .with_context(|| format!("digest line {}, block {h}", c.line))?;
            checked.push(h);
        } else {
            // The run's last block before a restart: its `since` values need this child's reply.
            checker.check_child(h, &hash).with_context(|| format!("block {h}"))?;
        }
    }
    Ok(Bip9Check {
        tip: get(&chain_info, "bestblockhash")?
            .as_hash()
            .ok_or_else(|| anyhow!("bestblockhash is not a hash"))?,
        checked,
    })
}

mod checks;

#[cfg(test)]
mod tests;
