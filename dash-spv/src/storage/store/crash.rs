//! The SPV store under ST3a's crash harness (SB13 acceptance, SB14 §3 Crash tests; SB10's
//! `faulty::spv` miniature with the real [`Store`] in its place).
//!
//! - The store is a real [`Store`] on the world's power-loss filesystem ([`FaultyIo`]: the
//!   segment files, `tip`, the directory entries of `block_headers/`, `spv/` and the storage
//!   directory) and a [`FaultyEngine`] as its SQLite, which exists only once `spv/kv.sqlite`
//!   does ([`FaultyOpener`], D4). Both crash at one cut point, as a process kill or a power
//!   loss; a nested crash during recovery is a kill, a reopen and a cut (r3 N3).
//! - Header steps go header steps through
//!   [`HeaderStep`](super::step::HeaderStep) (records of every branch, the `active` change
//!   with D44's inclusive `from_height`, `meta`), and three subscribers through the legacy
//!   writes (`put_sub`, `truncate_sub`), each `WriteSet` carrying its locator. Every step
//!   commits Buffered.
//! - The wallet has a database of its own, `wallet`, as the host's wallet does (DEC-112 O4: no
//!   wallet locator in the SPV store). Its `WriteSet` holds its rows and Core's block locator
//!   (`LocatorEntries`, chain.cpp:32-49 at dash develop 15db478750), and may name headers whose
//!   step is still Buffered in the SPV store.
//! - A boundary is the store's Durable barrier ([`Store::sync_wal`]), then the wallet's.
//! - Recovery: [`Store::open`] (SB14 §3 Open, D17) and [`Store::recover`] (SB0 §9.5's blind recovery), the anchor
//!   step when the store holds none, then the wallet's E3 rewind (SB0 E3): to the first entry of
//!   its locator on the active chain, as Core's `FindForkInGlobalIndex`, else to nothing.
//!
//! The checks: S1 on the stores as booted (the header graph, `active`, `meta.tip`, each
//! subscriber's rows along its locator's branch); after recovery S2 (every locator on the
//! active chain, its rows the active chain up to it) and S3 (each database recovered a state
//! of the workload at or after the acknowledged boundary, as recovery maps it); after R12's
//! continuation, S5.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use dash_chain::{
    ActiveDiff, AnchorState, BlockId, ChainMeta, ClPersisted, Durability, HeaderRecord, HeaderRow,
    HeaderStepSink, HeaderStore, MetaDelta, Status, StepRow, StepTail,
};
use dash_kv::faulty::spv::fnv;
use dash_kv::faulty::{
    allowed_tips, Abort, FaultyEngine, Hash, InvariantId, Mark, Recovery, Reference, Scenario,
    Seeded, Sut, Trace, Violation, World,
};
use dash_kv::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead};
use dash_primitives::BlockHash;

use super::hdr::io::FaultyIo;
use super::lazy::DbOpener;
use super::schema::{be32, prev_of, Subscriber, SPV_SCHEMA};
use super::{Error, Layout, Skip, Store, END, LOWER_BATCH};

mod tests;

/// The wallet's database: `meta` (its locator) and its rows, height → the block's hash.
pub static WALLET_SCHEMA: DbSchema = DbSchema {
    name: "wallet",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("w", CfProfile::Ordered)],
};

const WALLET_META: Cf = DbSchema::META;
const WALLET_ROWS: Cf = Cf::new(1);
const WALLET_LOCATOR: &[u8] = b"locator";
/// The continuation's mark in the store's legacy metadata: the tip it caught up to.
const DONE: &str = "crash-done";
/// An injected engine failure: the system stops (sanctioned).
const ABORT: &str = "SB13 store: a store call failed";

pub type Hash32 = [u8; 32];

/// A header hash, distinct per `(prev, height, tag)`: the same branch is the same headers.
pub fn header_hash(prev: &Hash32, height: u32, tag: u8) -> Hash32 {
    let mut h = [0xcc; 32];
    h[..8].copy_from_slice(&fnv(&[prev, &height.to_be_bytes(), &[tag]]).to_be_bytes());
    h[8..12].copy_from_slice(&height.to_be_bytes());
    h[12] = tag;
    h
}

/// The anchor: the record at height 0.
pub fn anchor() -> Hash32 {
    header_hash(&[0; 32], 0, 0)
}

/// The 80 bytes of the header `hash` at `height`: linked to `prev`, distinct per hash.
fn header_bytes(height: u32, prev: &Hash32, hash: &Hash32) -> [u8; 80] {
    let mut header = [0; 80];
    header[0] = 1;
    header[4..36].copy_from_slice(prev);
    header[36..40].copy_from_slice(&hash[..4]);
    header[68..72].copy_from_slice(&(1_000 + height).to_le_bytes());
    header[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
    header
}

fn record(id: u32, height: u32, prev: Hash32, hash: Hash32) -> HeaderRecord {
    let row = HeaderRow {
        hash,
        id,
        height,
        header: header_bytes(height, &prev, &hash),
        skip: None,
        status: Status::VALID_TREE,
        chain_work: [0; 32],
        time_max: 1_000 + height,
        kind: 0,
    };
    HeaderRecord::from_row(&row).expect("a well-formed row")
}

/// The heights of Core's block locator from `top` (chain.cpp:32-49): ten single steps, then
/// doubling ones, and always the anchor.
pub fn locator_heights(top: u32) -> Vec<u32> {
    let (mut out, mut step, mut h) = (Vec::new(), 1u32, top);
    loop {
        out.push(h);
        if h == 0 {
            return out;
        }
        h = h.saturating_sub(step);
        if out.len() > 10 {
            step *= 2;
        }
    }
}

/// A subscriber of the workload: the store's three, and the wallet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sub {
    Store(Subscriber),
    Wallet,
}

impl Sub {
    pub const ALL: [Sub; 4] = [
        Sub::Store(Subscriber::FilterHeaders),
        Sub::Store(Subscriber::Filters),
        Sub::Store(Subscriber::Blocks),
        Sub::Wallet,
    ];

    fn index(self) -> usize {
        match self {
            Sub::Store(s) => s as usize,
            Sub::Wallet => 3,
        }
    }

    fn letter(self) -> char {
        ['f', 'c', 'k', 'w'][self.index()]
    }

    /// A subscriber row: its letter and the block's hash.
    fn value(self, hash: &Hash32) -> Vec<u8> {
        [&[self.letter() as u8][..], hash].concat()
    }
}

/// Where a subscriber step goes: from the last height its path shares with `chain` (`None`:
/// it has none) to `upto` (capped at the tip, at least the fork). `None` when nothing changes.
pub fn plan(path: &[Hash32], chain: &[Hash32], upto: u32) -> Option<(Option<u32>, u32)> {
    let shared = path.iter().zip(chain).take_while(|(a, b)| a == b).count();
    let fork = shared.checked_sub(1).map(|f| f as u32);
    let tip = chain.len() as u32 - 1;
    let to = upto.min(tip).max(fork.unwrap_or(0));
    if path.len() == shared && fork.is_some_and(|f| f >= to) {
        return None;
    }
    Some((fork, to))
}

/// The chain after a header step: `chain` kept up to `at` (capped at the tip), then `n`
/// headers tagged `tag`.
pub fn extend(chain: &[Hash32], at: u32, n: u8, tag: u8) -> Vec<Hash32> {
    let mut out = chain[..=(at as usize).min(chain.len() - 1)].to_vec();
    for _ in 0..n {
        let h = out.len() as u32;
        out.push(header_hash(out.last().expect("the anchor"), h, tag));
    }
    out
}

/// The planted bugs, all in the wallet's host (the store has none to plant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bug {
    /// W1, R6-M2's hazard: the wallet's recovery infers its fork from heights, keeping its rows
    /// up to the store's tip height whatever their hashes.
    W1,
    /// W2: a boundary that does not sync the wallet's WAL.
    W2,
}

/// How headers reach the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderPath {
    /// `dash-chain`'s `HeaderStep` (D44's `ActiveDiff`): SB14's writer.
    Step,
    /// The legacy writes dash-spv ships until SB14 (D4): `put_headers`, and a reorg's
    /// `truncate_headers` as a `WriteSet` of its own.
    Legacy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub recovery: Recovery,
    pub bug: Option<Bug>,
    pub headers: HeaderPath,
    /// A sync removed from the store (a negative control).
    pub skip: Skip,
}

impl Default for Config {
    /// SQLite's: prefix recovery (`dash-kv-sqlite`'s `sqlite_wal_prefix`), no bug, the step.
    fn default() -> Self {
        Self {
            recovery: Recovery::Prefix,
            bug: None,
            headers: HeaderPath::Step,
            skip: Skip::default(),
        }
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let engine = match self.recovery {
            Recovery::Prefix => "prefix",
            Recovery::Subset => "subset",
        };
        let bug = match self.bug {
            None => "none",
            Some(Bug::W1) => "W1",
            Some(Bug::W2) => "W2",
        };
        let headers = match self.headers {
            HeaderPath::Step => "step",
            HeaderPath::Legacy => "legacy",
        };
        write!(f, "engine={engine} bug={bug} headers={headers}")?;
        if self.skip.dir_sync {
            write!(f, " skip=dirsync")?;
        }
        if self.skip.storage_sync {
            write!(f, " skip=storagesync")?;
        }
        Ok(())
    }
}

impl FromStr for Config {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let mut cfg = Self::default();
        for part in s.split_whitespace() {
            match part.split_once('=').ok_or(())? {
                ("engine", "prefix") => cfg.recovery = Recovery::Prefix,
                ("engine", "subset") => cfg.recovery = Recovery::Subset,
                ("bug", "none") => cfg.bug = None,
                ("bug", "W1") => cfg.bug = Some(Bug::W1),
                ("bug", "W2") => cfg.bug = Some(Bug::W2),
                ("headers", "step") => cfg.headers = HeaderPath::Step,
                ("headers", "legacy") => cfg.headers = HeaderPath::Legacy,
                ("skip", "dirsync") => cfg.skip.dir_sync = true,
                ("skip", "storagesync") => cfg.skip.storage_sync = true,
                _ => return Err(()),
            }
        }
        Ok(cfg)
    }
}

/// An operation of the workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `hN`: a header step extending the active chain by `n`.
    Extend {
        n: u8,
    },
    /// `rA+N:T`: a header step to a branch of `n` headers forking at height `at`, tagged `tag`.
    Reorg {
        at: u32,
        n: u8,
        tag: u8,
    },
    /// `tA`: the legacy reorg's first half, the active chain truncated to `at` (capped at the
    /// tip) in a `WriteSet` of its own; the branch follows as `rA+N:T` from the new tip.
    Truncate {
        at: u32,
    },
    /// `fU`, `cU`, `kU`, `wU`: a subscriber step to the active block at `upto` (capped at the
    /// tip), from where its rows leave the active chain.
    Sub {
        sub: Sub,
        upto: u32,
    },
    /// `b`: a boundary.
    Boundary,
    /// `C`: R12's continuation, every subscriber to the tip, then a boundary.
    Continue,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Extend {
                n,
            } => write!(f, "h{n}"),
            Self::Reorg {
                at,
                n,
                tag,
            } => write!(f, "r{at}+{n}:{tag}"),
            Self::Truncate {
                at,
            } => write!(f, "t{at}"),
            Self::Sub {
                sub,
                upto,
            } => write!(f, "{}{upto}", sub.letter()),
            Self::Boundary => f.write_str("b"),
            Self::Continue => f.write_str("C"),
        }
    }
}

impl FromStr for Op {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        fn num<T: FromStr>(s: &str) -> Result<T, ()> {
            s.parse().map_err(|_| ())
        }
        let positive = |s: &str| num(s).ok().filter(|&n: &u8| n > 0).ok_or(());
        match s {
            "b" => return Ok(Self::Boundary),
            "C" => return Ok(Self::Continue),
            _ => {}
        }
        let (head, rest) = s.split_at_checked(1).ok_or(())?;
        Ok(match head {
            "h" => Self::Extend {
                n: positive(rest)?,
            },
            "r" => {
                let (at, rest) = rest.split_once('+').ok_or(())?;
                let (n, tag) = rest.split_once(':').ok_or(())?;
                Self::Reorg {
                    at: num(at)?,
                    n: positive(n)?,
                    tag: num(tag)?,
                }
            }
            "t" => Self::Truncate {
                at: num(rest)?,
            },
            _ => Self::Sub {
                sub: *Sub::ALL.iter().find(|s| head.chars().eq([s.letter()])).ok_or(())?,
                upto: num(rest)?,
            },
        })
    }
}

/// The workload's state: the active chain and each subscriber's path (the chain from the
/// anchor to its top).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefState {
    pub chain: Vec<Hash32>,
    pub paths: [Vec<Hash32>; 4],
}

impl Default for RefState {
    fn default() -> Self {
        Self {
            chain: vec![anchor()],
            paths: Default::default(),
        }
    }
}

impl RefState {
    pub fn step(&self, op: Op) -> Self {
        let mut s = self.clone();
        match op {
            Op::Extend {
                n,
            } => s.chain = extend(&self.chain, u32::MAX, n, 0),
            Op::Reorg {
                at,
                n,
                tag,
            } => {
                s.rewind_store_subs(at);
                s.chain = extend(&self.chain, at, n, tag);
            }
            Op::Truncate {
                at,
            } => {
                s.rewind_store_subs(at);
                s.chain.truncate(at as usize + 1);
            }
            Op::Sub {
                sub,
                upto,
            } => {
                if let Some((_, to)) = plan(&self.paths[sub.index()], &self.chain, upto) {
                    s.paths[sub.index()] = self.chain[..=to as usize].to_vec();
                }
            }
            Op::Boundary | Op::Continue => {}
        }
        s
    }

    fn tip(&self) -> Hash32 {
        *self.chain.last().expect("the anchor")
    }

    /// D7: a lowering from `f = at + 1` (at or below the tip) rewinds the store's subscribers
    /// above `f − 1`, as its prefixes have them ([`lowering`](Self::lowering)), even where the
    /// new chain re-activates the records above that: a reorg to a branch whose first headers
    /// are already active. Recovery's walk to the branch's highest active ancestor would otherwise
    /// expect them higher than the store keeps them (sweep 0x5b14_2601).
    fn rewind_store_subs(&mut self, at: u32) {
        let f = (at as usize).saturating_add(1);
        if f < self.chain.len() {
            for p in &mut self.paths[..3] {
                p.truncate(f);
            }
        }
    }
}

/// The reference: the state after each op, the first before any; and the states a lowering
/// passes through (SB14 D7: each batch is a valid prefix), each with the index of the state of
/// its op.
pub struct Ref {
    pub states: Vec<RefState>,
    pub prefixes: Vec<(usize, RefState)>,
    /// The legacy writes, whose appends wait for a raise (the step path commits each step).
    pub legacy: bool,
}

impl RefState {
    /// The states an op's lowering passes through (D7): the batches from the top, each the
    /// chain below its lowest copied height, the subscribers rewound to the first replaced
    /// height − 1 with the first batch; the last is the chain below that height.
    pub fn lowering(&self, op: Op) -> Vec<RefState> {
        let tip = self.chain.len() as u32 - 1;
        let f = match op {
            Op::Reorg {
                at,
                ..
            } => at + 1,
            Op::Truncate {
                at,
            } => at + 1,
            _ => return Vec::new(),
        };
        if f > tip || f == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        // The store's subscribers: the store never rewinds the wallet (Opus-h r2 nit 2).
        let mut paths = self.paths.clone();
        for p in &mut paths[..3] {
            p.truncate(f as usize);
        }
        let mut top = tip;
        loop {
            let b = top.saturating_sub(LOWER_BATCH - 1).max(f);
            out.push(RefState {
                chain: self.chain[..b as usize].to_vec(),
                paths: paths.clone(),
            });
            if b == f {
                return out;
            }
            top = b - 1;
        }
    }
}

impl Reference for Ref {
    type Op = Op;
    type Config = Config;

    fn new(cfg: &Config) -> Self {
        Self {
            states: vec![RefState::default()],
            prefixes: Vec::new(),
            legacy: cfg.headers == HeaderPath::Legacy,
        }
    }

    fn apply(&mut self, op: &Op) {
        assert_ne!(*op, Op::Continue, "the continuation is R12's");
        let last = self.states.last().expect("the initial state");
        let next = last.step(*op);
        let at = self.states.len();
        // The wallet's path is not rewound by the store's lowering.
        for mut p in last.lowering(*op) {
            p.paths[3] = last.paths[3].clone();
            self.prefixes.push((at, p));
        }
        self.states.push(next);
    }
}

/// A failed store call: an injected engine failure (sanctioned) or anything else (a finding).
#[derive(Debug)]
pub enum Failed {
    Engine,
    Other(String),
}

impl From<Error> for Failed {
    fn from(e: Error) -> Self {
        match e {
            // An injected engine or file failure, or the writer it poisoned (D18): fail-stop.
            Error::Kv(_) | Error::Io(_) | Error::Poisoned(_) => Self::Engine,
            e => Self::Other(e.to_string()),
        }
    }
}

impl From<dash_chain::StoreError> for Failed {
    fn from(e: dash_chain::StoreError) -> Self {
        match e {
            dash_chain::StoreError::Io(_) => Self::Engine,
            e => Self::Other(e.to_string()),
        }
    }
}

impl From<dash_kv::StoreError> for Failed {
    fn from(_: dash_kv::StoreError) -> Self {
        Self::Engine
    }
}

type Res<T> = Result<T, Failed>;

/// The two databases.
pub struct System {
    pub store: Store<FaultyEngine, FaultyIo>,
    pub wallet: FaultyEngine,
    bug: Option<Bug>,
    headers: HeaderPath,
}

fn meta_delta(next_id: u32, tip: BlockId) -> MetaDelta {
    MetaDelta {
        next_id: BlockId(next_id),
        tip,
        best_header: tip,
    }
}

fn wallet_locator(chain: &[Hash32]) -> Vec<u8> {
    let top = chain.len() as u32 - 1;
    locator_heights(top)
        .into_iter()
        .flat_map(|h| [&be32(h)[..], &chain[h as usize]].concat())
        .collect()
}

fn decode_wallet_locator(v: &[u8]) -> Vec<(u32, Hash32)> {
    v.as_chunks::<36>()
        .0
        .iter()
        .map(|c| {
            (u32::from_be_bytes(c[..4].try_into().expect("4")), c[4..].try_into().expect("32"))
        })
        .collect()
}

impl System {
    /// Opens and recovers both databases.
    pub fn open(world: &World, wallet: FaultyEngine, cfg: &Config) -> Res<Self> {
        let store = open_store(world, cfg)?;
        store.recover()?;
        let sys = Self {
            store,
            wallet,
            bug: cfg.bug,
            headers: cfg.headers,
        };
        sys.init()?;
        sys.recover_wallet()?;
        Ok(sys)
    }

    /// The anchor's step on a store that holds none, Durable, then `Established` (the legacy
    /// path: the anchor's `put_headers`, then the store's barrier).
    fn init(&self) -> Res<()> {
        let view = self.store.view();
        if self.headers == HeaderPath::Legacy {
            if view.active.hi.is_none() {
                self.store.put_headers(0, &[(anchor(), header_bytes(0, &[0; 32], &anchor()))])?;
                self.store.sync_wal()?;
            }
        } else if view.meta.is_none() || view.active.hi.is_none() {
            self.store.reset(ChainMeta {
                schema: SPV_SCHEMA.version,
                anchor: (BlockHash::from(anchor()), 0),
                anchor_state: AnchorState::Candidate,
                next_id: BlockId(0),
                tip: BlockId(0),
                best_header: BlockId(0),
                sync_seq: 0,
                cl: ClPersisted {
                    best: None,
                    floor: None,
                    enabled_observed: false,
                },
            })?;
            let mut step = self.store.begin_step()?;
            let rows: Vec<StepRow> = record(0, 0, [0; 32], anchor()).step_rows(None).collect();
            step.rows(&rows)?;
            let tail = StepTail {
                active: Some(ActiveDiff {
                    from_height: 0,
                    new_tip: BlockId(0),
                    new_height: 0,
                    old_height: 0,
                }),
                cl: None,
                meta: meta_delta(1, BlockId(0)),
            };
            step.commit(&tail, Durability::Durable)?;
        }
        if self.store.view().meta.is_some_and(|m| m.anchor_state != AnchorState::Established) {
            self.store.establish()?;
        }
        Ok(())
    }

    /// E3: the wallet rewinds to the first entry of its locator on the active chain, or to
    /// nothing; one Durable `WriteSet`.
    fn recover_wallet(&self) -> Res<()> {
        self.wallet.sync_wal()?;
        let Some(v) = self.wallet.get(WALLET_META, WALLET_LOCATOR)? else {
            return Ok(());
        };
        let entries = decode_wallet_locator(&v);
        let top = entries.first().map_or(0, |e| e.0);
        let found = if self.bug == Some(Bug::W1) {
            // From heights: the lower of its top and the store's tip, whatever the hashes.
            let tip = self.chain()?.len() as u32 - 1;
            Some(top.min(tip))
        } else {
            let mut found = None;
            for (h, hash) in &entries {
                if self.store.active_hash(*h)? == Some(*hash) {
                    found = Some(*h);
                    break;
                }
            }
            found
        };
        if found == Some(top) {
            return Ok(());
        }
        let mut b = self.wallet.batch();
        match found {
            Some(h) => {
                b.delete_range(WALLET_ROWS, &be32(h + 1), &END);
                b.put(WALLET_META, WALLET_LOCATOR, &wallet_locator(&self.chain()?[..=h as usize]));
            }
            None => {
                b.delete_range(WALLET_ROWS, &[], &END);
                b.delete(WALLET_META, WALLET_LOCATOR);
            }
        }
        self.wallet.write(b, Commit::Durable)?;
        Ok(())
    }

    /// The active chain's hashes from the anchor.
    pub fn chain(&self) -> Res<Vec<Hash32>> {
        let hi = self.store.view().active.hi.ok_or(Failed::Other("no active chain".into()))?;
        Ok(self.store.headers(0..hi + 1)?.into_iter().map(|(h, _)| h).collect())
    }

    /// A subscriber's path: the hashes its rows name, from the anchor.
    pub fn path(&self, sub: Sub) -> Res<Vec<Hash32>> {
        let rows: Vec<Vec<u8>> = match sub {
            Sub::Store(s) => match self.store.view().sub(s).hi {
                Some(hi) => self.store.sub_range(s, 0..hi + 1)?,
                None => Vec::new(),
            },
            Sub::Wallet => self
                .wallet
                .iter_prefix(WALLET_ROWS, &[])
                .map(|r| r.map(|(_, v)| v))
                .collect::<dash_kv::Result<_>>()?,
        };
        rows.iter()
            .map(|v| {
                v.get(1..33)
                    .and_then(|h| h.try_into().ok())
                    .ok_or_else(|| Failed::Other(format!("a bad {} row", sub.letter())))
            })
            .collect()
    }

    /// A header step to `extend(chain, at, n, tag)`: new records for the headers the store
    /// does not hold, the `active` change from the lowest rewritten height (D44), `meta`. On
    /// the legacy path, `put_headers` at the tip: a reorg's truncation is its own op (`tA`), as
    /// two writes in one op would crash into states the reference does not model.
    pub fn header_step(&self, at: u32, n: u8, tag: u8) -> Res<()> {
        let chain = self.chain()?;
        let new = extend(&chain, at, n, tag);
        let keep = (at as usize).min(chain.len() - 1);
        if self.headers == HeaderPath::Legacy {
            if keep + 1 < chain.len() {
                return Err(Failed::Other("a legacy reorg needs its truncation (tA) first".into()));
            }
            let raw: Vec<_> = (keep + 1..new.len())
                .map(|h| (new[h], header_bytes(h as u32, &new[h - 1], &new[h])))
                .collect();
            return Ok(self.store.put_headers(keep as u32 + 1, &raw)?);
        }
        let meta = self.store.view().meta.ok_or(Failed::Other("no meta".into()))?;
        let mut next_id = meta.next_id.0;
        let mut rows = Vec::new();
        let id_of = |hash: &Hash32| -> Res<Option<BlockId>> {
            Ok(self.store.by_hash(&BlockHash::from(*hash))?.map(|r| r.id))
        };
        let mut prev = id_of(&new[keep])?.ok_or(Failed::Other("the fork is not stored".into()))?;
        for (h, hash) in new.iter().enumerate().skip(keep + 1) {
            prev = match id_of(hash)? {
                Some(id) => id,
                None => {
                    let rec = record(next_id, h as u32, new[h - 1], *hash);
                    rows.extend(rec.step_rows(Some(prev)));
                    next_id += 1;
                    BlockId(next_id - 1)
                }
            };
        }
        let mut step = self.store.begin_step()?;
        step.rows(&rows)?;
        let tail = StepTail {
            active: Some(ActiveDiff {
                from_height: keep as u32 + 1,
                new_tip: prev,
                new_height: new.len() as u32 - 1,
                old_height: chain.len() as u32 - 1,
            }),
            cl: None,
            meta: meta_delta(next_id, prev),
        };
        Ok(step.commit(&tail, Durability::Buffered)?)
    }

    /// A subscriber step to `upto`: a truncation to the fork when its rows left the active
    /// chain, then the rows above it with the locator (the store's subscribers: two
    /// `WriteSet`s; the wallet: one).
    pub fn sub_step(&self, sub: Sub, upto: u32) -> Res<()> {
        let chain = self.chain()?;
        let path = self.path(sub)?;
        let Some((fork, to)) = plan(&path, &chain, upto) else {
            return Ok(());
        };
        let from = fork.map_or(0, |f| f + 1);
        let values: Vec<Vec<u8>> =
            chain[from as usize..=to as usize].iter().map(|h| sub.value(h)).collect();
        match sub {
            Sub::Store(s) => {
                if path.len() as u32 > from {
                    let fork = fork.ok_or(Failed::Other("rows off the anchor".into()))?;
                    self.store.truncate_sub(s, fork)?;
                }
                let refs: Vec<&[u8]> = values.iter().map(|v| &v[..]).collect();
                self.store.put_sub(s, from, &refs)?;
            }
            Sub::Wallet => {
                let mut b = self.wallet.batch();
                b.delete_range(WALLET_ROWS, &be32(from), &END);
                for (h, v) in (from..).zip(&values) {
                    b.put(WALLET_ROWS, &be32(h), v);
                }
                b.put(WALLET_META, WALLET_LOCATOR, &wallet_locator(&chain[..=to as usize]));
                self.wallet.write(b, Commit::Buffered)?;
            }
        }
        Ok(())
    }

    /// A boundary: the store's Durable barrier, then the wallet's.
    pub fn boundary(&self) -> Res<()> {
        self.store.sync_wal()?;
        if self.bug != Some(Bug::W2) {
            self.wallet.sync_wal()?;
        }
        Ok(())
    }

    /// R12's continuation: every subscriber to the tip, marked done, then a boundary.
    pub fn catch_up(&self) -> Res<()> {
        for sub in Sub::ALL {
            self.sub_step(sub, u32::MAX)?;
        }
        let tip = *self.chain()?.last().expect("the anchor");
        self.store.put_metadata(DONE, &tip)?;
        self.boundary()
    }

    /// What the two databases hold.
    pub fn image(&self) -> Res<Image> {
        let mut records = std::collections::BTreeMap::new();
        let mut ids = std::collections::BTreeMap::new();
        for r in self.store.from_height(0)? {
            let row = r.to_row();
            records.insert(row.hash, (row.height, prev_of(&row)));
            if let Some(other) = ids.insert(row.id, row.hash) {
                return Err(Failed::Other(format!(
                    "id {} names two records ({:?}, {:?})",
                    row.id,
                    &other[..4],
                    &row.hash[..4]
                )));
            }
            let by_id = self.store.by_id(BlockId(row.id))?.map(|r| *r.hash.as_bytes());
            if by_id != Some(row.hash) {
                return Err(Failed::Other(format!("by_id({}) is not its record", row.id)));
            }
        }
        let meta = self.store.view().meta;
        let tip = match meta {
            Some(m) => self.store.by_id(m.tip)?.map(|r| r.to_row().hash),
            None => None,
        };
        let active = match self.store.view().active.hi {
            Some(_) => self.chain()?,
            None => Vec::new(),
        };
        let mut locs = [None; 4];
        let mut paths: [Vec<Hash32>; 4] = Default::default();
        for sub in Sub::ALL {
            paths[sub.index()] = self.path(sub)?;
            locs[sub.index()] = match sub {
                Sub::Store(s) => self.store.locator(s)?,
                Sub::Wallet => self
                    .wallet
                    .get(WALLET_META, WALLET_LOCATOR)?
                    .and_then(|v| decode_wallet_locator(&v).first().map(|&(h, x)| (x, h))),
            };
        }
        let done = self.store.metadata(DONE)?.and_then(|v| v.try_into().ok());
        Ok(Image {
            records,
            active,
            tip,
            locs,
            paths,
            done,
        })
    }
}

/// What the two databases hold, in the workload's terms.
#[derive(Debug, Clone, Default)]
pub struct Image {
    /// Every record: hash → (height, prev).
    pub records: std::collections::BTreeMap<Hash32, (u32, Hash32)>,
    /// The `active` projection's hashes from height 0.
    pub active: Vec<Hash32>,
    /// `meta.tip`'s hash.
    pub tip: Option<Hash32>,
    /// Each subscriber's locator: (hash, height).
    pub locs: [Option<(Hash32, u32)>; 4],
    /// Each subscriber's rows' hashes, from height 0.
    pub paths: [Vec<Hash32>; 4],
    pub done: Option<Hash32>,
}

impl Image {
    /// The chain from the anchor to `hash` through the stored records.
    fn ancestry(&self, hash: &Hash32) -> Option<Vec<Hash32>> {
        let (mut path, mut at) = (vec![*hash], *hash);
        while at != anchor() {
            at = self.records.get(&at)?.1;
            path.push(at);
            if path.len() > self.records.len() + 1 {
                return None;
            }
        }
        path.reverse();
        Some(path)
    }

    fn is_active(&self, hash: &Hash32, h: u32) -> bool {
        self.active.get(h as usize) == Some(hash)
    }

    /// Where a store subscriber at `path`'s top is after the store's recovery: kept when
    /// active, else at its branch's highest active ancestor through the records (`fork_of`),
    /// else reset.
    fn recovered_store_loc(&self, path: &[Hash32]) -> Option<(Hash32, u32)> {
        let (mut at, mut h) = (*path.last()?, path.len() as u32 - 1);
        loop {
            if self.is_active(&at, h) {
                return Some((at, h));
            }
            at = self.records.get(&at)?.1;
            h = h.checked_sub(1)?;
        }
    }

    /// Where the wallet at `path`'s top is after E3: the first entry of its locator on the
    /// active chain, else nothing.
    fn recovered_wallet_loc(&self, path: &[Hash32]) -> Option<(Hash32, u32)> {
        let top = path.len().checked_sub(1)? as u32;
        locator_heights(top)
            .into_iter()
            .map(|h| (path[h as usize], h))
            .find(|(x, h)| self.is_active(x, *h))
    }
}

/// S1, on the databases as booted.
pub fn check_stores(img: &Image) -> Vec<Violation> {
    let s1 = |d: String| Violation::new(InvariantId::S1, d);
    let mut v = Vec::new();
    // The header graph is closed: each record's parent is stored a height below it.
    for (hash, &(h, prev)) in &img.records {
        let parent_ok = if h == 0 {
            *hash == anchor()
        } else {
            img.records.get(&prev).is_some_and(|p| p.0 + 1 == h)
        };
        if !parent_ok {
            v.push(s1(format!("record at {h}: its parent is not stored below it")));
            break;
        }
    }
    // `active` is the chain to `meta.tip`.
    if !img.active.is_empty() {
        match img.tip.and_then(|t| img.ancestry(&t)) {
            Some(chain) if chain == img.active => {}
            _ => v.push(s1(format!(
                "`active` (to {}) is not the chain to meta.tip",
                img.active.len()
            ))),
        }
    }
    // Each subscriber's rows are its locator's branch, which is stored (the store's: in the
    // same database, committed after its header step).
    for sub in Sub::ALL {
        let (path, loc) = (&img.paths[sub.index()], img.locs[sub.index()]);
        let s = sub.letter();
        let top = path.last().map(|x| (*x, path.len() as u32 - 1));
        if loc != top {
            v.push(s1(format!("{s}'s locator {loc:?} is not its rows' top {top:?}")));
            continue;
        }
        if let (Sub::Store(_), Some((x, _))) = (sub, loc) {
            if img.ancestry(&x).as_deref() != Some(&path[..]) {
                v.push(s1(format!("{s}'s rows are not the stored branch to its locator")));
            }
        }
    }
    v
}

/// S2, S3 and S5, on the databases as recovered: `acked` is the step the last acknowledged
/// boundary made durable.
pub fn check_recovered(img: &Image, refm: &Ref, acked: usize) -> Vec<Violation> {
    let mut v = Vec::new();
    // S2: every locator is on the active chain, and its rows are the active chain up to it.
    for sub in Sub::ALL {
        let path = &img.paths[sub.index()];
        if !img.active.starts_with(path) {
            v.push(Violation::new(
                InvariantId::S2,
                format!("{}'s rows (to {}) leave the active chain", sub.letter(), path.len()),
            ));
        }
    }
    // The states at or after the acknowledged step, with the lowering prefixes of later ops,
    // each with the state index it belongs to.
    let from = acked.min(refm.states.len() - 1);
    let after: Vec<(usize, &RefState)> = (from..)
        .zip(&refm.states[from..])
        .chain(refm.prefixes.iter().filter(|(at, _)| *at > from).map(|(at, p)| (*at, p)))
        .collect();
    let tip = img.active.last().copied();
    if let Some(done) = img.done {
        // S5: the continuation caught every subscriber up to the tip.
        for sub in Sub::ALL {
            if img.locs[sub.index()].map(|l| l.0) != Some(done) || tip != Some(done) {
                v.push(Violation::new(
                    InvariantId::S5,
                    format!("after the continuation, {} is not at the tip", sub.letter()),
                ));
            }
        }
        if !after.iter().any(|(_, r)| Some(r.tip()) == tip) {
            v.push(Violation::new(
                InvariantId::S3,
                format!("the tip is no state's from the acknowledged step {acked} on"),
            ));
        }
        return v;
    }
    // S3: the store recovered one state at or after the acknowledged step (its tip, and each
    // subscriber as its recovery leaves it), and the wallet one, as E3 leaves it.
    let store = |r: &RefState| {
        let mut out = vec![Some((r.tip(), r.chain.len() as u32 - 1))];
        out.extend((0..3).map(|i| img.recovered_store_loc(&r.paths[i])));
        out
    };
    let got: Vec<_> = std::iter::once(tip.map(|t| (t, img.active.len() as u32 - 1)))
        .chain(img.locs[..3].iter().copied())
        .collect();
    // SB14 §3 "Drop without `stop`": legacy appends since the last raise are not committed,
    // and a crash may lose them while later SQLite commits (naming only committed heights)
    // survive. So a state may have its chain cut back to any length down to the newest durable
    // slot before its op: the acknowledged boundary's (a raise), or (D42) the latest earlier
    // op's lowering since, whose last batch is Durable (D7) and whose pre-sync made everything
    // below it durable, so a cut stops at its lowest prefix (Opus-h r1 n1: not the lowest of
    // every lowering since). A state's own lowering batches are prefixes, not cuts. The step
    // path commits each step: no cut. Subscribers naming a cut height then fail the comparison.
    let acked_len = refm.states[from].chain.len();
    let floor = |i: usize| {
        let lowered = || refm.prefixes.iter().filter(|(at, _)| (from + 1..i).contains(at));
        let last = lowered().map(|(at, _)| *at).max();
        lowered()
            .filter(|(at, _)| Some(*at) == last)
            .map(|(_, p)| p.chain.len())
            .min()
            .unwrap_or(acked_len)
    };
    let cut = |r: &RefState, len: usize| RefState {
        chain: r.chain[..len].to_vec(),
        paths: r.paths.clone(),
    };
    if !after.iter().any(|&(i, r)| {
        let lo = if refm.legacy {
            floor(i).min(r.chain.len()).max(1)
        } else {
            r.chain.len()
        };
        (lo..=r.chain.len()).any(|n| store(&cut(r, n)) == got)
    }) {
        v.push(Violation::new(
            InvariantId::S3,
            format!("the SPV store is in no state from the acknowledged step {acked} on: {got:?}"),
        ));
    }
    let w = img.locs[3];
    if !after.iter().any(|(_, r)| img.recovered_wallet_loc(&r.paths[3]) == w) {
        v.push(Violation::new(
            InvariantId::S3,
            format!("the wallet ({w:?}) is in no state from the acknowledged step {acked} on"),
        ));
    }
    v
}

/// A step count as a trace hash (the flush marks' tips).
fn step_hash(step: u64) -> Hash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&step.to_le_bytes());
    h[31] = 0x13;
    h
}

fn step_of(h: Hash) -> usize {
    u64::from_le_bytes(h[..8].try_into().expect("8 bytes")) as usize
}

fn engines(world: &World, cfg: &Config) -> (FaultyEngine, FaultyEngine) {
    let spv: Arc<FaultyEngine> = world.engine_with("spv", &SPV_SCHEMA, cfg.recovery);
    let wallet = world.engine_with("wallet", &WALLET_SCHEMA, cfg.recovery);
    ((*spv).clone(), (*wallet).clone())
}

/// The storage directory in the world's filesystem (its datadir).
const STORAGE: &str = dash_kv::faulty::DATADIR;
/// The segment size under the oracle: small, so the workload crosses segment files.
pub const PER: u32 = 4;

/// The store's SQLite under the oracle: the world's `spv` engine, which exists only while
/// `node/spv/kv.sqlite` does (D4: only `ENOENT` means absent). Creating it creates `spv/` and
/// the file, syncs `spv/` (SQLite's `syncDir`), and starts the engine empty: a database whose
/// directory entry a power loss took is gone, whatever rows the engine kept.
pub struct FaultyOpener {
    world: World,
    recovery: Recovery,
}

impl FaultyOpener {
    fn file() -> std::path::PathBuf {
        std::path::Path::new(STORAGE).join("spv").join("kv.sqlite")
    }

    fn engine(&self) -> FaultyEngine {
        (*self.world.engine_with("spv", &SPV_SCHEMA, self.recovery)).clone()
    }
}

impl DbOpener<FaultyEngine> for FaultyOpener {
    fn open_existing(&self) -> super::Result<Option<FaultyEngine>> {
        use dash_kv::FlatFs as _;
        Ok(self.world.fs().len(&Self::file())?.map(|_| self.engine()))
    }

    fn create(&self) -> super::Result<FaultyEngine> {
        use dash_kv::FlatFs as _;
        let fs = self.world.fs();
        let dir = std::path::Path::new(STORAGE).join("spv");
        fs.create_dir_all(&dir)?;
        fs.create(&Self::file())?;
        fs.sync_dir(&dir)?;
        let engine = self.engine();
        let mut b = engine.batch();
        for cf in SPV_SCHEMA.all_cfs() {
            b.delete_range(cf, &[], &END);
        }
        super::kv(engine.write(b, Commit::Durable))?;
        Ok(engine)
    }
}

/// The store on the world, with `cfg`'s syncs removed.
fn open_store(world: &World, cfg: &Config) -> Res<Store<FaultyEngine, FaultyIo>> {
    let store = Store::open(Layout {
        storage: std::path::PathBuf::from(STORAGE),
        opener: Box::new(FaultyOpener {
            world: world.clone(),
            recovery: cfg.recovery,
        }),
        io: Arc::new(FaultyIo(world.fs())),
        per: PER,
    })?;
    store.set_skip(cfg.skip);
    Ok(store)
}

fn fail(world: &World, f: Failed) -> Abort {
    let message = match f {
        Failed::Engine => ABORT.to_string(),
        Failed::Other(m) => format!("SB13 store refused: {m}"),
    };
    world.mark(Mark::StoreFailed {
        op: "store",
    });
    world.mark(Mark::Abort {
        message: message.clone(),
    });
    Abort::new(message)
}

/// The system under test.
pub struct StoreSut {
    world: World,
    sys: System,
    applied: u64,
    flushes: u64,
    acked: u64,
}

impl Sut for StoreSut {
    type Config = Config;
    type Op = Op;
    type Ref = Ref;
    const NAME: &'static str = "sb13-spv-store";

    fn open(world: &World, cfg: &Config) -> Result<Self, Abort> {
        world.mark(Mark::RecoveryBegin);
        let (_, wallet) = engines(world, cfg);
        let sys = System::open(world, wallet, cfg).map_err(|f| fail(world, f))?;
        world.mark(Mark::RecoveryEnd);
        Ok(Self {
            world: world.clone(),
            sys,
            applied: 0,
            flushes: 0,
            acked: 0,
        })
    }

    fn apply(&mut self, op: &Op) -> Result<(), Abort> {
        self.applied += 1;
        let done = match *op {
            Op::Boundary => {
                self.flushes += 1;
                let flush = self.flushes;
                self.world.mark(Mark::FlushBegin {
                    flush,
                    old: Some(step_hash(self.acked)),
                    new: step_hash(self.applied),
                });
                let done = self.sys.boundary();
                if done.is_ok() {
                    self.acked = self.applied;
                    self.world.mark(Mark::Durable {
                        flush,
                    });
                }
                done
            }
            Op::Continue => self.sys.catch_up(),
            Op::Extend {
                n,
            } => self.sys.header_step(u32::MAX, n, 0),
            Op::Reorg {
                at,
                n,
                tag,
            } => self.sys.header_step(at, n, tag),
            Op::Truncate {
                at,
            } => self.sys.store.truncate_headers(at).map_err(Failed::from),
            Op::Sub {
                sub,
                upto,
            } => self.sys.sub_step(sub, upto),
        };
        done.map_err(|f| fail(&self.world, f))
    }

    fn check(&self, _: &World, refm: &Ref, trace: &Trace) -> Result<(), Vec<Violation>> {
        let img = self.sys.image().map_err(|f| {
            vec![Violation::new(InvariantId::S2, format!("the stores do not read back: {f:?}"))]
        })?;
        let acked = allowed_tips(trace).0.map_or(0, step_of);
        let v = check_recovered(&img, refm, acked);
        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
        }
    }

    fn store_check(world: &World, cfg: &Config) -> Vec<Violation> {
        let (_, wallet) = engines(world, cfg);
        let image = open_store(world, cfg).and_then(|store| {
            System {
                store,
                wallet,
                bug: None,
                headers: cfg.headers,
            }
            .image()
        });
        match image {
            Ok(img) => check_stores(&img),
            Err(f) => {
                vec![Violation::new(InvariantId::S1, format!("the stores do not read: {f:?}"))]
            }
        }
    }

    fn continue_after_recovery(&mut self) -> Option<Op> {
        Some(Op::Continue)
    }

    fn has_continuation() -> bool {
        true
    }

    fn sanctioned(_: &Config, message: &str) -> bool {
        message == ABORT
    }

    fn projection(&self) -> Option<Hash> {
        let img = self.sys.image().ok()?;
        let locs: Vec<Vec<u8>> = img
            .locs
            .iter()
            .map(|l| l.map_or(Vec::new(), |(x, h)| [&x[..], &h.to_be_bytes()].concat()))
            .collect();
        let tip = img.active.last().copied().unwrap_or_default();
        Some(step_hash(fnv(&[&tip, &locs[0], &locs[1], &locs[2], &locs[3]])))
    }
}

/// The exhaustive workload: two boundaries; between them header steps each subscriber follows,
/// a reorg they follow from where they leave the chain, and wallet steps naming headers whose
/// step is still Buffered in the store.
pub const WORKLOAD: &str = "h2 f2 c2 w2 b h2 f4 w4 h1 r1+3:1 c9 f9 k9 w9 b h1 w9 f9";

/// The same workload on the legacy path: the reorg is the truncation, then the branch.
pub const LEGACY_WORKLOAD: &str = "h2 f2 c2 w2 b h2 f4 w4 h1 t1 r1+3:1 c9 f9 k9 w9 b h1 w9 f9";

/// A random workload (the seeded sweep's).
pub fn random_scenario(rng: &mut Seeded) -> Scenario<StoreSut> {
    let headers = [HeaderPath::Step, HeaderPath::Legacy][rng.below(2) as usize];
    let mut st = RefState::default();
    let mut ops = Vec::new();
    for _ in 0..4 + rng.below(9) {
        let tip = st.chain.len() as u64 - 1;
        let op = match rng.below(10) {
            0..=2 => Op::Extend {
                n: 1 + rng.below(3) as u8,
            },
            3 if tip > 0 => {
                let at = rng.below(tip) as u32;
                if headers == HeaderPath::Legacy {
                    let truncate = Op::Truncate {
                        at,
                    };
                    st = st.step(truncate);
                    ops.push(truncate);
                }
                Op::Reorg {
                    at,
                    n: 1 + rng.below(3) as u8,
                    tag: 1 + rng.below(3) as u8,
                }
            }
            4..=7 => Op::Sub {
                sub: Sub::ALL[rng.below(4) as usize],
                upto: rng.below(tip + 2) as u32,
            },
            _ => Op::Boundary,
        };
        st = st.step(op);
        ops.push(op);
    }
    let cfg = Config {
        headers,
        ..Config::default()
    };
    Scenario::new("sb13-sweep", cfg, ops)
}
