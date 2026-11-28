//! Collect the reference chain and node observations for `mixnet-watch`. The append-only
//! log is also the restart state. Driver events enter the log between collection passes, and
//! divergences write `PAGE-<height>-<kind>.json` beside the log once (optionally invoking
//! `--page-cmd`).
//! `mixnet-watch --nodes <config> --out <log> [--events <path|->] [--poll-ms <n>] [--page-cmd <cmd>]`
//! exits 0 after event EOF, 1 for an output failure, or 2 for invalid input.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use dashcore_rpc::{Auth, Client};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::contract::*;
use super::criteria::{self, Page, PageKind};
use crate::{NodeRpc, RpcError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(rename = "v")]
    _version: V1,
    run_id: String,
    run: Run,
    llmq_params: Vec<LlmqParams>,
    nodes: Vec<Node>,
    coinbase: BTreeMap<String, SlotId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    slot: SlotId,
    rpc: SocketAddr,
    cookie: PathBuf,
    runs: Impl,
    chainlocks: bool,
    chaintips: bool,
}

impl Node {
    fn client(&self) -> Result<Client, RpcError> {
        Ok(Client::new(&format!("http://{}", self.rpc), Auth::CookieFile(self.cookie.clone()))?)
    }
}

impl Config {
    fn validate(&self) -> Result<(), RpcError> {
        if !self
            .nodes
            .first()
            .is_some_and(|n| n.slot == SlotId::CONTROLLER && n.runs == Impl::Dashd)
        {
            return Err("first node must be node0 running dashd".into());
        }
        let mut slots = BTreeSet::new();
        for node in &self.nodes {
            if !slots.insert(node.slot) {
                return Err(format!("duplicate slot {}", node.slot).into());
            }
            if node.chainlocks && node.runs == Impl::Rust {
                return Err(format!("chainlocks enabled for Rust slot {}", node.slot).into());
            }
        }
        if self.coinbase.values().any(|s| !slots.contains(s)) {
            return Err("coinbase refers to an unknown slot".into());
        }
        check_log(&[Envelope {
            v: V1,
            run: self.run_id.clone(),
            seq: 0,
            mono_ns: 0,
            src: "watch".into(),
            record: Record::Run(self.run.clone()),
        }])?;
        Ok(())
    }
}

fn field<T: DeserializeOwned>(value: &Value, name: &str) -> Result<T, RpcError> {
    T::deserialize(&value[name]).map_err(|e| format!("{name}: {e}").into())
}

fn array<'a>(value: &'a Value, name: &str) -> Result<&'a [Value], RpcError> {
    value[name].as_array().map(Vec::as_slice).ok_or_else(|| format!("expected {name} array").into())
}

fn pose_rows(diff: &Value, height: u32, baseline: bool) -> Result<Vec<Record>, RpcError> {
    let mut rows = Vec::new();
    for mn in array(diff, "addedMNs")? {
        let state = &mn["state"];
        rows.push(Record::MnDiff(MnDiff {
            height,
            pro_tx: field(mn, "proTxHash")?,
            pose_penalty: Some(field(state, "PoSePenalty")?),
            pose_ban_height: Some(field(state, "PoSeBanHeight")?),
            pose_revived_height: Some(field(state, "PoSeRevivedHeight")?),
        }));
    }
    if !baseline {
        for update in array(diff, "updatedMNs")? {
            for (id, state) in update.as_object().ok_or("expected an updatedMNs object")? {
                let state = state.as_object().ok_or("expected updated masternode fields")?;
                let pose_penalty = state.get("PoSePenalty").map(u32::deserialize).transpose()?;
                let pose_ban_height =
                    state.get("PoSeBanHeight").map(i32::deserialize).transpose()?;
                let pose_revived_height =
                    state.get("PoSeRevivedHeight").map(i32::deserialize).transpose()?;
                if pose_penalty.is_some()
                    || pose_ban_height.is_some()
                    || pose_revived_height.is_some()
                {
                    rows.push(Record::MnDiff(MnDiff {
                        height,
                        pro_tx: id.parse()?,
                        pose_penalty,
                        pose_ban_height,
                        pose_revived_height,
                    }));
                }
            }
        }
    }
    Ok(rows)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Quorum {
    llmq_type: u8,
    quorum_hash: Hex32,
    quorum_index: u16,
    signers_count: u32,
    valid_members_count: u32,
    quorum_vvec_hash: Hex32,
}

impl Quorum {
    fn null(&self) -> bool {
        self.signers_count == 0 && self.valid_members_count == 0
    }

    fn commitment(self, mined_height: u32, info: &Value) -> Result<Record, RpcError> {
        let mut members = Vec::new();
        if !self.null() {
            for m in array(info, "members")? {
                members.push(Member {
                    pro_tx: field(m, "proTxHash")?,
                    valid: field(m, "valid")?,
                });
            }
        }
        Ok(Record::Commitment(Commitment {
            null: self.null(),
            llmq_type: self.llmq_type,
            quorum_hash: self.quorum_hash,
            index: self.quorum_index,
            base_height: field(info, "height")?,
            mined_height,
            members,
            vvec_hash: self.quorum_vvec_hash,
        }))
    }
}

fn block_data(config: &Config, block: &Value) -> Result<(Block, Vec<Quorum>), RpcError> {
    let txs = array(block, "tx")?;
    let outputs = array(txs.first().ok_or("missing coinbase")?, "vout")?;
    let miner = outputs
        .iter()
        .filter_map(|o| config.coinbase.get(o["scriptPubKey"]["address"].as_str()?))
        .find_map(|slot| config.nodes.iter().find(|n| &n.slot == slot))
        .map(|n| Miner {
            slot: n.slot,
            runs: n.runs,
        });
    let mut quorums = Vec::new();
    for tx in txs {
        if field::<u16>(tx, "type")? == 6 {
            quorums.push(Quorum::deserialize(&tx["qcTx"]["commitment"])?);
        }
    }
    Ok((
        Block {
            height: field(block, "height")?,
            hash: field(block, "hash")?,
            prev: field(block, "previousblockhash")?,
            miner,
        },
        quorums,
    ))
}

fn get<T: DeserializeOwned>(
    rpc: &impl NodeRpc,
    method: &str,
    args: &[Value],
) -> Result<T, RpcError> {
    Ok(T::deserialize(rpc.call(method, args)?)?)
}

fn block_hash(rpc: &impl NodeRpc, height: u32) -> Result<Hex32, RpcError> {
    get(rpc, "getblockhash", &[height.into()])
}

fn collect(
    config: &Config,
    rpc: &impl NodeRpc,
    h: u32,
    baseline: bool,
    chain: &BTreeMap<u32, Hex32>,
) -> Result<Vec<Record>, RpcError> {
    let hash = block_hash(rpc, h)?;
    let body = rpc.call("getblock", &[hash.as_str().into(), 2.into()])?;
    let (block, quorums) = block_data(config, &body)?;
    if block.hash != hash
        || block.height != h
        || chain.get(&h).is_some_and(|recorded| *recorded != hash)
        || chain.get(&(h - 1)).is_some_and(|prev| *prev != block.prev)
    {
        return Err(format!("reference chain changed at {h}").into());
    }
    let mut records = vec![Record::Block(block)];
    for qc in quorums {
        let hash = qc.quorum_hash.as_str().into();
        let info = if qc.null() {
            rpc.call("getblockheader", &[hash])?
        } else {
            rpc.call("quorum", &["info".into(), qc.llmq_type.into(), hash])?
        };
        records.push(qc.commitment(h, &info)?);
    }
    let base = if baseline {
        1
    } else {
        h - 1
    };
    let diff = rpc.call("protx", &["listdiff".into(), base.into(), h.into()])?;
    records.extend(pose_rows(&diff, h, baseline)?);
    if block_hash(rpc, h)? != hash {
        return Err(format!("reference chain changed while collecting {h}").into());
    }
    Ok(records)
}

fn polls(node: &Node, rpc: &impl NodeRpc) -> Result<Vec<Record>, RpcError> {
    let slot = node.slot;
    let hash: Hex32 = get(rpc, "getbestblockhash", &[])?;
    let height = field(&rpc.call("getblockheader", &[hash.as_str().into()])?, "height")?;
    let mut records = vec![Record::Tip(Tip {
        node: slot,
        height,
        hash,
    })];
    if node.chaintips {
        let value = rpc.call("getchaintips", &[])?;
        let mut tips = Vec::new();
        for t in value.as_array().ok_or("expected getchaintips array")? {
            tips.push(ChainTip {
                height: field(t, "height")?,
                hash: field(t, "hash")?,
                branchlen: field(t, "branchlen")?,
                status: field(t, "status")?,
            });
        }
        records.push(Record::ChainTips(ChainTips {
            node: slot,
            tips,
        }));
    }
    if node.chainlocks
        && let Ok(cl) = rpc.call("getbestchainlock", &[])
    {
        records.push(Record::ClSeen(ClSeen {
            node: slot,
            height: field(&cl, "height")?,
            hash: field(&cl, "blockhash")?,
        }));
    }
    Ok(records)
}

fn monotonic_ns() -> io::Result<u64> {
    let mut ts = std::mem::MaybeUninit::uninit();
    // clock_gettime initializes the timespec on success.
    let ts = unsafe {
        if libc::clock_gettime(libc::CLOCK_MONOTONIC, ts.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        ts.assume_init()
    };
    Ok(ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64)
}

#[derive(Debug)]
pub enum Error {
    Input(String),
    Output(io::Error),
    Retry(RpcError),
}

impl Error {
    fn input(e: impl fmt::Display) -> Self {
        Self::Input(e.to_string())
    }
}

impl From<RpcError> for Error {
    fn from(e: RpcError) -> Self {
        Self::Retry(e)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(s) => s.fmt(f),
            Self::Output(e) => e.fmt(f),
            Self::Retry(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Default)]
struct State {
    last: Option<u64>,
    ends_with_watermark: bool,
    params: usize,
    chain: BTreeMap<u32, Hex32>,
    h0: Option<u32>,
    next_height: Option<u64>,
    pending: Vec<Record>,
    tips: BTreeMap<SlotId, Tip>,
    chaintips: BTreeMap<SlotId, ChainTips>,
    locks: BTreeMap<SlotId, ClSeen>,
}

impl State {
    fn apply(&mut self, e: &Envelope) {
        self.last = Some(e.seq);
        self.ends_with_watermark = matches!(e.record, Record::Watermark(_));
        match &e.record {
            Record::LlmqParams(_) => self.params += 1,
            Record::Reorg(r) => {
                self.chain.retain(|h, _| *h <= r.fork_height);
                self.next_height = Some(u64::from(r.fork_height) + 1);
                self.pending.clear();
            }
            Record::Watermark(w) => {
                self.h0.get_or_insert(w.height);
                self.next_height = Some(u64::from(w.height) + 1);
                self.pending.clear();
            }
            Record::Block(b) => {
                self.next_height.get_or_insert(u64::from(b.height));
                self.chain.insert(b.height, b.hash.clone());
                self.pending.push(e.record.clone());
            }
            Record::MnDiff(_) | Record::Commitment(_) => self.pending.push(e.record.clone()),
            Record::Tip(t) => {
                self.tips.insert(t.node, t.clone());
            }
            Record::ChainTips(t) => {
                self.chaintips.insert(t.node, t.clone());
            }
            Record::ClSeen(t) => {
                self.locks.insert(t.node, t.clone());
            }
            _ => {}
        }
    }

    fn seen_poll(&self, r: &Record) -> bool {
        match r {
            Record::Tip(t) => self.tips.get(&t.node) == Some(t),
            Record::ChainTips(t) => self.chaintips.get(&t.node) == Some(t),
            Record::ClSeen(t) => self.locks.get(&t.node) == Some(t),
            _ => false,
        }
    }
}

#[derive(Serialize)]
struct PageFile<'a> {
    run: &'a str,
    kind: PageKind,
    height: u32,
    detail: &'a str,
    seq: u64,
}

fn page_kind_name(kind: PageKind) -> &'static str {
    match kind {
        PageKind::DigestMismatch => "digest-mismatch",
        PageKind::BestHash => "best-hash",
        PageKind::ClConflict => "cl-conflict",
        PageKind::Pose => "pose",
    }
}

fn run_page_cmd(cmd: &OsStr, path: &Path) {
    match Command::new(cmd)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let _ = writeln!(
                io::stderr().lock(),
                "mixnet-watch: page command failed ({status}) for {}",
                path.display()
            );
        }
        Err(e) => {
            let _ = writeln!(
                io::stderr().lock(),
                "mixnet-watch: page command failed to run for {}: {e}",
                path.display()
            );
        }
    }
}

fn write_page(
    dir: &Path,
    run: &str,
    page: &Page,
    seq: u64,
    page_cmd: Option<&OsStr>,
) -> Result<(), Error> {
    let name = format!("PAGE-{}-{}.json", page.height, page_kind_name(page.kind));
    let final_path = dir.join(&name);
    if final_path.try_exists().map_err(Error::Output)? {
        return Ok(());
    }
    let tmp_path = dir.join(format!(".{name}.tmp"));
    let mut bytes = serde_json::to_vec(&PageFile {
        run,
        kind: page.kind,
        height: page.height,
        detail: &page.detail,
        seq,
    })
    .map_err(|e| Error::Output(io::Error::other(e)))?;
    bytes.push(b'\n');
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(Error::Output)?;
        file.write_all(&bytes).map_err(Error::Output)?;
        file.sync_all().map_err(Error::Output)?;
    }
    let linked = match fs::hard_link(&tmp_path, &final_path) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            return Err(Error::Output(e));
        }
    };
    fs::remove_file(&tmp_path).map_err(Error::Output)?;
    if linked && let Some(cmd) = page_cmd {
        run_page_cmd(cmd, &final_path);
    }
    Ok(())
}

struct Log {
    file: File,
    dir: PathBuf,
    run: String,
    state: State,
    entries: Vec<Envelope>,
    checked: usize,
    last_metrics_err: Option<String>,
}

impl Log {
    fn open(path: &Path, config: &Config) -> Result<Self, Error> {
        let name = path.file_name().map(OsStr::as_encoded_bytes).unwrap_or_default();
        let name = name.to_ascii_uppercase();
        if name.starts_with(b"PAGE-") || name.starts_with(b".PAGE-") {
            return Err(Error::input("--out names a page file"));
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(Error::Output)?;
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let mut state = State::default();
        let mut entries = Vec::new();
        let mut offset = 0;
        let mut line = Vec::new();
        let size = file.metadata().map_err(Error::input)?.len();
        let mut reader = BufReader::new((&file).take(size));
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line).map_err(Error::input)?;
            if n == 0 {
                break;
            }
            if line.last() != Some(&b'\n') {
                let run_id = serde_json::to_string(&config.run_id).map_err(Error::input)?;
                let prefix = format!(r#"{{"v":1,"run":{run_id}"#);
                if !prefix.as_bytes().starts_with(&line) && !line.starts_with(prefix.as_bytes()) {
                    return Err(Error::input("unrecognized trailing fragment in log"));
                }
                file.set_len(offset).map_err(Error::Output)?;
                break;
            }
            let e: Envelope = serde_json::from_slice(&line).map_err(Error::input)?;
            if e.run != config.run_id {
                return Err(Error::input("log run id differs from nodes.json"));
            }
            if let Some(last) = state.last {
                if e.seq <= last || matches!(e.record, Record::Run(_)) {
                    return Err(Error::input(format!(
                        "invalid log sequence or second Run at byte {offset}"
                    )));
                }
            } else {
                check_log(std::slice::from_ref(&e)).map_err(Error::input)?;
            }
            state.apply(&e);
            entries.push(e);
            offset += n as u64;
        }
        let mut log = Self {
            file,
            dir,
            run: config.run_id.clone(),
            state,
            entries,
            checked: 0,
            last_metrics_err: None,
        };
        if log.state.last.is_none() {
            log.watch(Record::Run(config.run.clone()))?;
        }
        if log.state.h0.is_none() {
            for p in config.llmq_params.iter().skip(log.state.params) {
                log.watch(Record::LlmqParams(p.clone()))?;
            }
        }
        Ok(log)
    }

    fn write(&mut self, record: Record, src: String, mono_ns: u64) -> Result<(), Error> {
        let seq = match self.state.last {
            None => 1,
            Some(n) => n.checked_add(1).ok_or_else(|| Error::input("log sequence exhausted"))?,
        };
        let e = Envelope {
            v: V1,
            run: self.run.clone(),
            seq,
            mono_ns,
            src,
            record,
        };
        let mut line = serde_json::to_vec(&e).map_err(|e| Error::Output(io::Error::other(e)))?;
        line.push(b'\n');
        if self.file.write(&line).map_err(Error::Output)? != line.len() {
            return Err(Error::Output(io::ErrorKind::WriteZero.into()));
        }
        self.state.apply(&e);
        self.entries.push(e);
        Ok(())
    }

    fn watch(&mut self, record: Record) -> Result<(), Error> {
        self.write(record, "watch".into(), monotonic_ns().map_err(Error::Output)?)
    }

    fn check_pages(&mut self, page_cmd: Option<&OsStr>) -> Result<(), Error> {
        if self.entries.len() == self.checked {
            return Ok(());
        }
        self.checked = self.entries.len();
        let metrics = match criteria::metrics(&self.entries, &criteria::Config::default()) {
            Ok(m) => m,
            Err(e) => {
                if self.last_metrics_err.as_deref() != Some(e.as_str()) {
                    let _ = writeln!(io::stderr().lock(), "mixnet-watch: metrics: {e}");
                    self.last_metrics_err = Some(e);
                }
                return Ok(());
            }
        };
        let seq = self.state.last.unwrap_or(0);
        for page in &metrics.pages {
            write_page(&self.dir, &self.run, page, seq, page_cmd)?;
        }
        Ok(())
    }

    fn pass(
        &mut self,
        config: &Config,
        rpc: &impl NodeRpc,
        final_pass: bool,
    ) -> Result<bool, Error> {
        let t: u32 = get(rpc, "getblockcount", &[])?;
        let mut reorg = false;
        if let Some((&height, old_tip)) = self.state.chain.last_key_value() {
            let (mut fork, mut hash) = (height, old_tip.clone());
            while fork > t || hash != block_hash(rpc, fork)? {
                fork =
                    fork.checked_sub(1).ok_or_else(|| Error::input("reference genesis changed"))?;
                hash = match self.state.chain.get(&fork) {
                    Some(parent) => parent.clone(),
                    // A fork can predate the first block in the log.
                    None => field(
                        &rpc.call("getblockheader", &[hash.as_str().into()])?,
                        "previousblockhash",
                    )?,
                };
            }
            if fork < height {
                self.watch(Record::Reorg(Reorg {
                    fork_height: fork,
                    old_tip: old_tip.clone(),
                    new_tip: get(rpc, "getbestblockhash", &[])?,
                }))?;
                reorg = true;
            }
        }
        if t == 0 {
            return Ok(false);
        }
        let next = self.state.next_height.get_or_insert(u64::from(t));
        if self.state.h0.is_none() && self.state.chain.is_empty() {
            *next = (*next).min(u64::from(t));
        }
        let next = *next;
        for h in next..=u64::from(t) {
            let h = h as u32;
            let records = collect(
                config,
                rpc,
                h,
                h == 1 || self.state.h0.is_none_or(|h0| h <= h0),
                &self.state.chain,
            )?;
            let partial = std::mem::take(&mut self.state.pending);
            for r in records {
                if !partial.contains(&r) {
                    self.watch(r)?;
                }
            }
            self.watch(Record::Watermark(Watermark {
                height: h,
            }))?;
        }
        for (i, node) in config.nodes.iter().enumerate() {
            let records = if i == 0 {
                polls(node, rpc)?
            } else if let Ok(rows) = node.client().and_then(|client| polls(node, &client)) {
                rows
            } else {
                continue;
            };
            for r in records {
                if !self.state.seen_poll(&r) {
                    self.watch(r)?;
                }
            }
        }
        if (final_pass && !self.state.ends_with_watermark) || (reorg && next > u64::from(t)) {
            self.watch(Record::Watermark(Watermark {
                height: t,
            }))?;
        }
        Ok(true)
    }

    /// Returns true when the driver has closed its stream.
    fn receive_events(
        &mut self,
        rx: &mpsc::Receiver<Result<Event, RpcError>>,
        interval: Duration,
    ) -> Result<bool, Error> {
        let started = Instant::now();
        loop {
            let elapsed = started.elapsed();
            if elapsed >= interval {
                return Ok(false);
            }
            match rx.recv_timeout(interval - elapsed) {
                Ok(e) => {
                    let e = e.map_err(Error::input)?;
                    self.write(e.record, e.src, e.mono_ns)?;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => return Ok(false),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(true),
            }
        }
    }
}

struct Event {
    src: String,
    mono_ns: u64,
    record: Record,
}

fn event(line: &str) -> Result<Event, RpcError> {
    let mut obj: serde_json::Map<String, Value> = serde_json::from_str(line)?;
    let src =
        obj.remove("src").map(String::deserialize).transpose()?.unwrap_or_else(|| "driver".into());
    let mono_ns = match obj.remove("mono_ns") {
        Some(n) => u64::deserialize(n)?,
        None => monotonic_ns()?,
    };
    let record = Record::deserialize(Value::Object(obj))?;
    if matches!(
        record,
        Record::Run(_)
            | Record::LlmqParams(_)
            | Record::Block(_)
            | Record::Reorg(_)
            | Record::Watermark(_)
            | Record::Tip(_)
            | Record::ChainTips(_)
            | Record::Commitment(_)
            | Record::MnDiff(_)
            | Record::ClSeen(_)
    ) {
        return Err("driver event has a watcher-owned kind".into());
    }
    Ok(Event {
        src,
        mono_ns,
        record,
    })
}

fn read_events(path: PathBuf) -> mpsc::Receiver<Result<Event, RpcError>> {
    let (tx, rx) = mpsc::sync_channel(128);
    thread::spawn(move || {
        let read = || -> Result<(), RpcError> {
            let reader: Box<dyn BufRead> = if path == Path::new("-") {
                Box::new(io::stdin().lock())
            } else {
                Box::new(BufReader::new(File::open(path)?))
            };
            for line in reader.lines() {
                if tx.send(Ok(event(&line?)?)).is_err() {
                    break;
                }
            }
            Ok(())
        };
        if let Err(e) = read() {
            let _ = tx.send(Err(e));
        }
    });
    rx
}

/// Runs until the events stream ends and the reference tip has been collected.
pub fn run(
    nodes: &Path,
    out: &Path,
    events: Option<PathBuf>,
    interval: Duration,
    page_cmd: Option<&OsStr>,
) -> Result<(), Error> {
    let config: Config =
        serde_json::from_slice(&fs::read(nodes).map_err(Error::input)?).map_err(Error::input)?;
    config.validate().map_err(Error::input)?;
    let mut log = Log::open(out, &config)?;
    let mut events = events.map(read_events);
    let mut eof = false;
    loop {
        let pass = config.nodes[0]
            .client()
            .map_err(Error::from)
            .and_then(|rpc| log.pass(&config, &rpc, eof));
        let done = match pass {
            Ok(done) => done && eof,
            Err(Error::Retry(e)) => {
                let _ = writeln!(io::stderr().lock(), "mixnet-watch: retrying pass: {e}");
                false
            }
            Err(e) => return Err(e),
        };
        log.check_pages(page_cmd)?;
        if done {
            return Ok(());
        }
        let Some(rx) = &events else {
            thread::sleep(interval);
            continue;
        };
        if log.receive_events(rx, interval)? {
            events = None;
            eof = true;
        }
    }
}

#[cfg(test)]
mod tests;
