//! The first-mismatch dump (spec O8 §10.4; node program task O9a).
//!
//! A dump is a directory `<root>/<chain>/<label>/<height>-<hash prefix>/` holding:
//! * `mismatch.json`: the mismatch (heights, hashes, kinds, origins, each differing key with its
//!   class and both values), both inputs with their SHA-256, and the report's verdict and
//!   uncovered keys;
//! * `left.jsonl`, `right.jsonl`: the inputs' lines verbatim around the mismatch (± `context`
//!   lines), the record compared before it, and in linear mode every other record of the same
//!   block (failed attempts, disconnects, VerifyDB);
//! * `decisions-left.jsonl`, `decisions-right.jsonl`: the `A` record of the mismatching attempt,
//!   for a digest input given its decisions file (joined by run and `seq`, spec §7.3);
//! * `sparse-left.jsonl`, `sparse-right.jsonl`: for a golden input, the nearest `protx_list`,
//!   `protx_listdiff`, `quorum_listextended`, `rotationinfo` and `creditpool` records at or
//!   below the height;
//! * `rpc/`: with a frozen copy's RPC, the same calls at exactly the height and the block's UTXO
//!   delta from `getblock <hash> 2`;
//! * `README.txt`: the text report, values included.
//!
//! **Where dumps go** (PLAN §7.5): by default `$DASH_EMBARGO_DIR/statedigest`. Mainnet and
//! testnet dumps go nowhere else: without `DASH_EMBARGO_DIR`, or with a `--dump-dir` outside it,
//! [`root`] refuses. A mainnet or testnet comparison without `DASH_EMBARGO_DIR` is refused
//! even without a dump ([`check_private`]). `DASH_EMBARGO_DIR` must name an existing directory by an absolute path (an
//! empty or relative value would put the private corpus under the current directory). Regtest and devnet dumps may go to any `--dump-dir` (task scratch).
//! Directories are created 0700 and files 0600, and an existing dump is never overwritten.

use std::collections::BTreeMap;
use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use dash_statedigest::compare::{At, Mode, Report, Side, What};
use dash_statedigest::{Chain, FileKind, Hash256, Record, Value};
use serde_json::json;

use super::compare::{Input, render};
use super::store::{Records, sha256_file};
use crate::rpc::{Auth, Rpc};

/// The `S` records a dump shows from a golden input (spec §10.4).
const SPARSE: &[&str] =
    &["protx_list", "protx_listdiff", "quorum_listextended", "rotationinfo", "creditpool"];

/// The JSON-RPC ports of the shared mainnet and testnet oracles. The dump's `quorum *` calls are
/// not pure reads, so it refuses them; `--rpc-url` must be a frozen copy.
const ORACLE_PORTS: &[u16] = &[27998, 37998];
/// The oracles' data directories, which hold their cookies.
const ORACLE_CHAINS: &str = "/work/chains";

/// `DASH_EMBARGO_DIR`, checked and resolved: an absolute path to an existing directory.
fn private_corpus(dir: &Path) -> Result<PathBuf> {
    if !dir.is_absolute() {
        bail!("DASH_EMBARGO_DIR {:?} is not an absolute path", dir.display().to_string());
    }
    let resolved = std::fs::canonicalize(dir)
        .with_context(|| format!("resolving DASH_EMBARGO_DIR {}", dir.display()))?;
    if !resolved.is_dir() {
        bail!("DASH_EMBARGO_DIR {} is not a directory", dir.display());
    }
    Ok(resolved)
}

/// Refuses a mainnet or testnet comparison without a valid `DASH_EMBARGO_DIR`, whether or not
/// it dumps (PLAN §7.5). Regtest and devnet need none.
pub fn check_private(chain: Chain, embargo_dir: Option<&Path>) -> Result<()> {
    match chain {
        Chain::Main | Chain::Test => match embargo_dir {
            Some(dir) => private_corpus(dir).map(drop),
            None => bail!(
                "a {} comparison needs the private corpus (PLAN §7.5): set DASH_EMBARGO_DIR",
                chain.as_str()
            ),
        },
        Chain::Regtest | Chain::Devnet => Ok(()),
    }
}

/// The directory dumps of `chain` go under (see the module docs).
pub fn root(chain: Chain, dump_dir: Option<&Path>, embargo_dir: Option<&Path>) -> Result<PathBuf> {
    let embargo = embargo_dir.map(private_corpus).transpose()?;
    match chain {
        Chain::Main | Chain::Test => {
            let Some(embargo) = embargo else {
                bail!(
                    "a {} comparison dumps to the private corpus only (PLAN §7.5): set \
                     DASH_EMBARGO_DIR",
                    chain.as_str()
                );
            };
            match dump_dir {
                None => Ok(embargo.join("statedigest")),
                Some(dir) if inside(dir, &embargo)? => Ok(dir.to_owned()),
                Some(dir) => bail!(
                    "{} is outside DASH_EMBARGO_DIR: {} dumps stay in the private corpus \
                     (PLAN §7.5)",
                    dir.display(),
                    chain.as_str()
                ),
            }
        }
        Chain::Regtest | Chain::Devnet => match (dump_dir, embargo) {
            (Some(dir), _) => Ok(dir.to_owned()),
            (None, Some(embargo)) => Ok(embargo.join("statedigest")),
            (None, None) => bail!("give --dump-dir, set DASH_EMBARGO_DIR, or pass --no-dump"),
        },
    }
}

/// Whether `dir` is `parent` or below it, resolving symlinks of the parts that exist.
fn inside(dir: &Path, parent: &Path) -> Result<bool> {
    let parent = std::fs::canonicalize(parent)
        .with_context(|| format!("resolving DASH_EMBARGO_DIR {}", parent.display()))?;
    let mut existing = dir.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return Ok(false);
        };
        rest.push(name.to_owned());
        if !existing.pop() {
            return Ok(false);
        }
        if existing.as_os_str().is_empty() {
            existing = PathBuf::from(".");
        }
    }
    let mut resolved = std::fs::canonicalize(&existing)?;
    for name in rest.iter().rev() {
        if name == ".." {
            return Ok(false);
        }
        resolved.push(name);
    }
    Ok(resolved.starts_with(&parent))
}

/// A frozen copy to query for the dump.
pub struct RpcTarget {
    pub url: String,
    pub auth: Auth,
}

impl RpcTarget {
    /// Refuses the shared oracles: their RPC use is pure-read, and the dump calls `quorum *`.
    pub fn new(url: String, auth: Auth) -> Result<RpcTarget> {
        // The port of `scheme://host:port/path`.
        let authority = url.split_once("://").map_or(url.as_str(), |(_, rest)| rest);
        let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
        let port = authority.rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok());
        if port.is_some_and(|p| ORACLE_PORTS.contains(&p)) {
            bail!("{url} is a shared oracle's port: the dump queries frozen copies only");
        }
        if let Auth::Cookie(path) = &auth
            && std::fs::canonicalize(path)
                .unwrap_or_else(|_| path.clone())
                .starts_with(ORACLE_CHAINS)
        {
            bail!("{} is an oracle's cookie: the dump queries frozen copies only", path.display());
        }
        Ok(RpcTarget {
            url,
            auth,
        })
    }
}

/// What the dump reads besides the two inputs.
pub struct Extras {
    /// Lines of context on each side of the mismatching record.
    pub context: usize,
    /// The decisions file of each digest input (left, right).
    pub decisions: [Option<PathBuf>; 2],
    pub rpc: Option<RpcTarget>,
}

fn create_dir(path: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("creating {}", path.display()))
}

fn create_dir_new(path: &Path) -> Result<()> {
    DirBuilder::new()
        .mode(0o700)
        .create(path)
        .with_context(|| format!("creating {}", path.display()))
}

fn write_json(dir: &Path, name: &str, value: &serde_json::Value) -> Result<()> {
    let mut text = serde_json::to_vec_pretty(value)?;
    text.push(b'\n');
    write_file(dir, name, &text)
}

fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let path = dir.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(bytes).with_context(|| format!("writing {}", path.display()))
}

/// `label` with every character outside `[A-Za-z0-9._-]` replaced by `_`.
pub fn sanitize(label: &str) -> String {
    label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn value_json(value: Option<&Value>) -> serde_json::Value {
    match value {
        None => json!({"absent": true}),
        Some(v) => serde_json::from_slice(&v.to_vec())
            .unwrap_or_else(|_| json!({"canonical": String::from_utf8_lossy(&v.to_vec())})),
    }
}

fn at_json(at: Option<&At>) -> serde_json::Value {
    at.map_or(serde_json::Value::Null, |at| {
        json!({
            "line": at.at,
            "k": (at.kind.letter() as char).to_string(),
            "h": at.id.h,
            "b": at.id.b.to_hex(),
            "org": at.id.org.as_str(),
        })
    })
}

/// `left` or `right`.
pub fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

/// `linear` or `stream`.
pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Linear => "linear",
        Mode::Stream => "stream",
    }
}

/// Writes the dump of `report`'s first mismatch under `root` and returns its directory.
pub fn write(
    root: &Path,
    label: &str,
    chain: Chain,
    inputs: &[Input; 2],
    report: &Report,
    extras: &Extras,
) -> Result<PathBuf> {
    let m = report.first().context("the report has no mismatch")?;
    let parent = root.join(chain.as_str()).join(sanitize(label));
    create_dir(&parent)?;
    let base = format!("{}-{}", m.h, &m.b().to_hex()[..16]);
    let dir = (0..)
        .map(|n| {
            parent.join(if n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            })
        })
        .find(|d| !d.exists())
        .expect("a free name");
    create_dir_new(&dir)?;

    let mut sides = Vec::new();
    for (i, input) in inputs.iter().enumerate() {
        sides.push(json!({
            "path": input.path.display().to_string(),
            "file": input.file.as_str(),
            "sha256": sha256_file(&input.path)?,
            "decisions": extras.decisions[i].as_ref().map(|p| p.display().to_string()),
        }));
    }
    let (what, keys) = match &m.what {
        What::Gap(side) => {
            (format!("gap: the {} input lacks the record", side_name(*side)), Vec::new())
        }
        What::Identity => ("identity".to_owned(), Vec::new()),
        What::Keys(diffs) => (
            "keys".to_owned(),
            diffs
                .iter()
                .map(|d| {
                    json!({
                        "field": d.field.to_string(),
                        "class": d.field.spec().class.key(),
                        "left": value_json(d.left.as_ref()),
                        "right": value_json(d.right.as_ref()),
                    })
                })
                .collect(),
        ),
    };
    let summary = json!({
        "mode": mode_name(report.mode),
        "chain": chain.as_str(),
        "inputs": sides,
        "h": m.h,
        "b": m.b().to_hex(),
        "what": what,
        "keys": keys,
        "left": at_json(m.left.as_ref()),
        "right": at_json(m.right.as_ref()),
        "before": [at_json(m.before[0].as_ref()), at_json(m.before[1].as_ref())],
        "verdict": report.verdict.exit_code(),
        "pairs": report.pairs,
        "uncovered": report.uncovered.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "skipped": report
            .skipped
            .iter()
            .map(|s| json!({"side": side_name(s.side), "from": s.from, "to": s.to}))
            .collect::<Vec<_>>(),
    });
    write_json(&dir, "mismatch.json", &summary)?;

    let sides = [(&m.left, Side::Left), (&m.right, Side::Right)];
    for (i, (input, (here, side))) in inputs.iter().zip(sides).enumerate() {
        let (here, name) = (here.as_ref(), side_name(side));
        // Each side's own block: after an identity mismatch the two differ.
        let b = here.map_or(m.b(), |at| &at.id.b);
        let lines = excerpt(input, b, here, m.before[i].as_ref(), report.mode, extras.context)?;
        write_file(&dir, &format!("{name}.jsonl"), &lines)?;
        if let (Some(at), Some(decisions)) = (here, &extras.decisions[i])
            && input.file == FileKind::Digest
        {
            let a = attempt_line(&input.path, decisions, at.at)?;
            write_file(&dir, &format!("decisions-{name}.jsonl"), &a.unwrap_or_default())?;
        }
        if input.file == FileKind::Golden {
            write_file(&dir, &format!("sparse-{name}.jsonl"), &sparse(&input.path, m.h)?)?;
        }
    }
    if let Some(rpc) = &extras.rpc {
        let rpc_dir = dir.join("rpc");
        create_dir_new(&rpc_dir)?;
        for (name, value) in rpc_detail(rpc, m.h, m.b()) {
            write_json(&rpc_dir, &format!("{name}.json"), &value)?;
        }
    }
    write_file(&dir, "README.txt", render(report, inputs, true).as_bytes())?;
    Ok(dir)
}

/// The lines of the excerpt, each prefixed with its number and a tab: ± `context` lines around
/// this side's mismatching record (for a gap on this side, around the record compared before
/// it), that earlier record, and in linear mode every other record of the block `b`.
fn excerpt(
    input: &Input,
    b: &Hash256,
    here: Option<&At>,
    before: Option<&At>,
    mode: Mode,
    context: usize,
) -> Result<Vec<u8>> {
    let center = here.or(before).map(|a| a.at);
    let mut out = Vec::new();
    for item in Records::raw(&input.path)? {
        let (line, bytes) = item?;
        let near = center.is_some_and(|c| line + context >= c && line <= c + context);
        let is_before = before.is_some_and(|a| a.at == line);
        let same_block = || {
            mode == Mode::Linear
                && dash_statedigest::parse_line(&bytes)
                    .is_ok_and(|r| r.block().is_some_and(|(_, rb)| rb == b))
        };
        if near || is_before || same_block() {
            write!(out, "{line}\t")?;
            out.extend_from_slice(&bytes);
        }
    }
    Ok(out)
}

/// The `A` line joined with the digest record on `line`: the same run string and `seq`
/// (spec §7.3).
fn attempt_line(digest: &Path, decisions: &Path, line: usize) -> Result<Option<Vec<u8>>> {
    let mut run: Option<Vec<u8>> = None;
    let mut seq = None;
    for item in Records::open(digest)? {
        let (n, record) = item?;
        match &record {
            Record::Header(h) => run = Some(h.run.clone()),
            Record::Outcome(o) if n == line => {
                seq = o.x().get("seq").and_then(Value::as_int);
                break;
            }
            _ => {}
        }
    }
    let (Some(run), Some(seq)) = (run, seq) else {
        return Ok(None);
    };
    let mut in_run = false;
    for item in Records::raw(decisions)? {
        let (n, bytes) = item?;
        match dash_statedigest::parse_line(&bytes) {
            Ok(Record::Header(h)) => in_run = h.run == run,
            Ok(Record::Attempt(a))
                if in_run && a.x().get("seq").and_then(Value::as_int) == Some(seq) =>
            {
                return Ok(Some(bytes));
            }
            Ok(_) => {}
            Err(e) => bail!("{}:{n}: {e}", decisions.display()),
        }
    }
    Ok(None)
}

/// The nearest `S` record at or below `h` for each of [`SPARSE`].
fn sparse(golden: &Path, h: i64) -> Result<Vec<u8>> {
    let mut nearest: BTreeMap<&'static str, (i64, Vec<u8>)> = BTreeMap::new();
    for item in Records::raw(golden)? {
        let (_, bytes) = item?;
        if let Ok(Record::Sparse(s)) = dash_statedigest::parse_line(&bytes)
            && s.h <= h
            && let Some(&name) = SPARSE.iter().find(|n| n.as_bytes() == s.rpc)
            && nearest.get(name).is_none_or(|(at, _)| s.h >= *at)
        {
            nearest.insert(name, (s.h, bytes));
        }
    }
    Ok(nearest.into_values().flat_map(|(_, line)| line).collect())
}

/// The frozen copy's answers at `h` (spec §10.4), each call's error recorded in its place.
fn rpc_detail(target: &RpcTarget, h: i64, b: &Hash256) -> Vec<(&'static str, serde_json::Value)> {
    let node = Rpc::new(target.url.clone(), target.auth.clone());
    let call = |method: &str, params: serde_json::Value| match node.try_call(method, params) {
        Ok(Ok(v)) => v,
        Ok(Err(code)) => json!({"rpc_error": code}),
        Err(e) => json!({"error": format!("{e:#}")}),
    };
    let block = call("getblock", json!([b.to_hex(), 2]));
    let mut out = vec![
        ("getblockhash", call("getblockhash", json!([h]))),
        ("protx_list", call("protx", json!(["list", "registered", true, h]))),
        ("quorum_listextended", call("quorum", json!(["listextended", h]))),
        ("creditpool", call("getcreditpoolinfo", json!([h]))),
    ];
    if h > 0 {
        out.push(("protx_listdiff", call("protx", json!(["listdiff", h - 1, h]))));
    }
    out.push(("utxo_delta", utxo_delta(&block)));
    out.push(("getblock", block));
    out
}

/// The outpoints a verbose block spends and the outputs it creates.
fn utxo_delta(block: &serde_json::Value) -> serde_json::Value {
    let Some(txs) = block["tx"].as_array() else {
        return json!({"error": "no verbose block"});
    };
    let mut spent = Vec::new();
    let mut created = Vec::new();
    for tx in txs {
        let txid = &tx["txid"];
        for vin in tx["vin"].as_array().into_iter().flatten() {
            if vin.get("coinbase").is_none() {
                spent.push(json!({"txid": vin["txid"], "vout": vin["vout"]}));
            }
        }
        for vout in tx["vout"].as_array().into_iter().flatten() {
            created.push(json!({
                "txid": txid,
                "n": vout["n"],
                "value": vout["value"],
                "script": vout["scriptPubKey"]["hex"],
            }));
        }
    }
    json!({"spent": spent, "created": created})
}
