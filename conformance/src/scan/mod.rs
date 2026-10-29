//! O6: one-off scans of a chain's blocks for the shapes R4 §2.3.5 (a)–(f) names, to see which
//! Dash Core quirks history actually exercises.
//!
//! A scan is one decode pass over a Core `blocks/` directory (read-only, see [`run`]). The
//! classifiers in [`classify`] are pure functions of a block's bytes, its height and the
//! network's parameters; they parse the three payloads they read by hand ([`payload`]) and share
//! no code with the libraries the harness otherwise tests. The oracle only supplies the chain
//! anchor and, for scan (c), the block reward.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use dashcore::hashes::{Hash, sha256d};
use serde_json::{Value, json};

pub use self::classify::{CClass, Mutant, c_hit, cl_sequence_hits, classify_c, scan_block};
pub use self::params::{Net, NetParams};
pub use self::run::{ScanOptions, ScanSummary, run_scan};
use crate::check::{H256, rev_hex};

pub mod classify;
pub mod params;
pub mod payload;
mod run;
#[cfg(test)]
mod tests;

/// A block's merkle root over its txids (`ComputeMerkleRoot`: an odd level repeats its last
/// node). All zero for no transactions.
pub fn merkle_root(txids: &[H256]) -> H256 {
    let mut level = txids.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(level[level.len() - 1]);
        }
        level = level
            .chunks(2)
            .map(|pair| sha256d::Hash::hash(&[pair[0], pair[1]].concat()).to_byte_array())
            .collect();
    }
    level.first().copied().unwrap_or([0; 32])
}

/// One of R4 §2.3.5's scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scan {
    A,
    B,
    C,
    D,
    E,
    F,
}

impl Scan {
    pub const ALL: [Scan; 6] = [Scan::A, Scan::B, Scan::C, Scan::D, Scan::E, Scan::F];

    pub fn letter(self) -> char {
        match self {
            Scan::A => 'a',
            Scan::B => 'b',
            Scan::C => 'c',
            Scan::D => 'd',
            Scan::E => 'e',
            Scan::F => 'f',
        }
    }

    pub fn from_letter(c: char) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.letter() == c.to_ascii_lowercase())
    }
}

/// A set of scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanSet(u8);

impl ScanSet {
    pub fn all() -> Self {
        Self::of(&Scan::ALL)
    }

    pub fn of(scans: &[Scan]) -> Self {
        Self(scans.iter().fold(0, |m, s| m | 1 << *s as u8))
    }

    pub fn contains(self, s: Scan) -> bool {
        self.0 & (1 << s as u8) != 0
    }

    /// Parses `a,b,f` or `abf`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut set = Vec::new();
        for c in s.chars().filter(|c| *c != ',') {
            set.push(Scan::from_letter(c).ok_or_else(|| format!("no scan {c:?}; use a-f"))?);
        }
        if set.is_empty() {
            return Err("no scans selected".into());
        }
        Ok(Self::of(&set))
    }

    pub fn letters(self) -> String {
        Scan::ALL.into_iter().filter(|s| self.contains(*s)).map(Scan::letter).collect()
    }
}

/// The state of the `v24` deployment over the scanned range, which decides whether `CheckCbTx`
/// requires the unlock-root version (`evo/cbtx.cpp:45-50`: `is_v24_active && isV20`).
/// `is_v24_active` is `DeploymentActiveAfter(pindexPrev, DEPLOYMENT_V24)` (`validation.cpp:6806`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum V24 {
    /// Not known (no oracle): the rule is not judged.
    Unknown,
    /// The deployment is not enabled on the network (`NEVER_ACTIVE`, which `getdeploymentinfo`
    /// leaves out): never active.
    NeverActive,
    /// Not active at the anchor tip, so at no block up to it.
    Inactive,
    /// Active for the blocks from this height on.
    ActiveFrom(u32),
}

impl V24 {
    /// `is_v24_active` for the block at `h`; `None` when not known.
    pub fn active_at(self, h: u32) -> Option<bool> {
        match self {
            V24::Unknown => None,
            V24::NeverActive | V24::Inactive => Some(false),
            V24::ActiveFrom(from) => Some(h >= from),
        }
    }

    pub fn to_json(self) -> Value {
        match self {
            V24::Unknown => json!({"state": "unknown"}),
            V24::NeverActive => json!({"state": "never active"}),
            V24::Inactive => json!({"state": "inactive at the anchor tip"}),
            V24::ActiveFrom(from) => json!({"state": "active", "from_height": from}),
        }
    }

    /// Reads the state out of a `getdeploymentinfo` reply: a deployment that is not listed is
    /// not enabled (`rpc/blockchain.cpp`, `SoftForkDescPushBack` returns early unless
    /// `DeploymentEnabled`); a listed one carries `active` and, once active, the `height` of the
    /// first block it applies to.
    pub fn from_deployment_info(info: &Value) -> Result<V24, String> {
        let Some(v24) = info.get("deployments").and_then(|d| d.get("v24")) else {
            info.get("deployments").ok_or("no `deployments` in the reply")?;
            return Ok(V24::NeverActive);
        };
        match v24.get("active").and_then(Value::as_bool) {
            Some(true) => v24
                .get("height")
                .and_then(Value::as_u64)
                .and_then(|h| u32::try_from(h).ok())
                .map(V24::ActiveFrom)
                .ok_or_else(|| "v24 is active but has no `height`".to_string()),
            Some(false) => Ok(V24::Inactive),
            None => Err("v24 has no `active`".into()),
        }
    }
}

/// What the classifiers run against: the network and the scans.
pub struct Cfg {
    pub net: NetParams,
    pub scans: ScanSet,
    /// The `v24` deployment state; [`V24::Unknown`] until the run learns it.
    pub v24: V24,
    /// `bestCLSignature` decode results by bytes: miners carry the same ChainLock forward.
    sig_cache: Mutex<HashMap<[u8; 96], payload::BlsState>>,
    #[cfg(test)]
    pub mutant: Option<Mutant>,
}

impl Cfg {
    pub fn new(net: Net, scans: ScanSet) -> Self {
        Self {
            net: NetParams::of(net),
            scans,
            v24: V24::Unknown,
            sig_cache: Mutex::default(),
            #[cfg(test)]
            mutant: None,
        }
    }

    /// Whether the mutant test has flipped predicate `m`. Always false outside test builds.
    #[inline]
    fn mutated(&self, m: Mutant) -> bool {
        #[cfg(test)]
        {
            self.mutant == Some(m)
        }
        #[cfg(not(test))]
        {
            let _ = m;
            false
        }
    }
}

/// One block to classify.
pub struct BlockInput<'a> {
    pub bytes: &'a [u8],
    pub height: u32,
    pub hash: H256,
    /// Not on the best chain.
    pub stale: bool,
    /// The median time past of the block's parent, for the best chain's blocks; called only
    /// where a verdict needs it. `None` for a block off the best chain.
    pub mtp_prev: Option<&'a (dyn Fn() -> Option<i64> + Sync)>,
}

/// One finding of a scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub scan: Scan,
    pub class: &'static str,
    pub height: u32,
    pub block: H256,
    pub tx_index: Option<u32>,
    pub txid: Option<H256>,
    pub stale: bool,
    pub detail: Value,
}

impl Hit {
    /// A best-chain hit on the block's coinbase.
    fn on_coinbase(
        scan: Scan,
        class: &'static str,
        height: u32,
        block: H256,
        txid: H256,
        detail: Value,
    ) -> Self {
        Self {
            scan,
            class,
            height,
            block,
            tx_index: Some(0),
            txid: Some(txid),
            stale: false,
            detail,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "class": self.class,
            "height": self.height,
            "block": rev_hex(&self.block),
            "tx_index": self.tx_index,
            "txid": self.txid.as_ref().map(rev_hex),
            "stale": self.stale,
            "detail": self.detail,
        })
    }

    fn sort_key(&self) -> (u32, u32, &'static str, bool) {
        (self.height, self.tx_index.unwrap_or(u32::MAX), self.class, self.stale)
    }
}

/// Sorts hits so two runs write the same bytes, whatever the thread count.
pub fn sort_hits(hits: &mut [Hit]) {
    hits.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()).then_with(|| a.block.cmp(&b.block)));
}

/// A best-chain block's coinbase ChainLock, for the sequential part of scan (d).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClFact {
    pub height: u32,
    pub block: H256,
    pub txid: H256,
    pub diff: u32,
    pub sig_valid: bool,
    /// `GetNonNullCoinbaseChainlock` would return it (evo/cbtx.cpp:181-211).
    pub non_null: bool,
}

/// Which kind of height scan (c) looks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CRowKind {
    /// An old-budget window block.
    Window,
    /// A superblock height.
    Superblock,
}

/// A scan (c) candidate: the coinbase's value, to be set against the block reward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CRow {
    pub height: u32,
    pub block: H256,
    pub kind: CRowKind,
    /// `vtx[0]->GetValueOut()`.
    pub coinbase_value: u64,
    pub coinbase_txid: H256,
    /// The blk file number and the offset of the block's record (its magic), where known.
    pub blk: Option<(u32, u64)>,
}

/// What classifying one block yields.
#[derive(Default, Debug)]
pub struct BlockResult {
    pub hits: Vec<Hit>,
    pub counters: BTreeMap<String, u64>,
    pub cl: Option<ClFact>,
    pub c_row: Option<CRow>,
    /// Why the block could not be framed, if it could not.
    pub unparsed: Option<String>,
}

/// Adds `n` to the counter `key`.
fn bump(counters: &mut BTreeMap<String, u64>, key: impl Into<String>, n: u64) {
    *counters.entry(key.into()).or_default() += n;
}

/// Adds every counter of `from` to `into`.
fn merge_counters(into: &mut BTreeMap<String, u64>, from: BTreeMap<String, u64>) {
    for (key, n) in from {
        bump(into, key, n);
    }
}

impl BlockResult {
    fn add(&mut self, key: &str, n: u64) {
        bump(&mut self.counters, key, n);
    }

    /// Stale blocks count under `stale.` and take no part in the sequential scans.
    fn finish(&mut self, stale: bool) {
        if stale {
            self.counters = std::mem::take(&mut self.counters)
                .into_iter()
                .map(|(k, v)| (format!("stale.{k}"), v))
                .collect();
            self.cl = None;
            self.c_row = None;
        }
    }
}
