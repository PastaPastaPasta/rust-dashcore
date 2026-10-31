//! `SpvSut`: SB0 §9.5's SPV store in miniature, with planted bugs B1–B7 (SB10 plan §5).
//!
//! - The database `spv` holds every header ever accepted (`hdr`: hash → height ‖ prev, never
//!   deleted), the active projection (`active`: height → hash), `meta.tip`, and two
//!   subscribers: `f`, filter-like, with a row per height, and `m`, MN-like, with keyed rows (the
//!   newest block of each height residue mod 3). A wallet-like subscriber `w` has a database of
//!   its own, `wallet`, as the wallet does: its locator can name a header the SPV store lost
//!   (§9.5, DEC-115 R6-M2), which is what recovery's rewind is for.
//! - A header step is one `WriteSet` (its `hdr` rows, the `active` change and `meta.tip`). A
//!   subscriber step is one `WriteSet` with its rows and its locator, written after the header
//!   step whose block it names (commit-before-publish). Steps commit Buffered.
//! - A boundary (checkpoint, sync-complete or stop) commits the SPV store Durably, then syncs the
//!   wallet's WAL.
//! - Recovery is blind: it syncs both WALs, reads the locators, and rewinds a subscriber whose
//!   locator names an unknown header to the anchor, its rows and locator in one Durable
//!   `WriteSet`.
//!
//! The anchor is implicit: height 0, never stored. Every `WriteSet` also writes a digest of its
//! part of the store (`meta.dg:*`), so a torn one shows (S4).
//!
//! The checks: S1 and S4 on the stores as booted, S2 and S3 after recovery, and S5 after R12's
//! continuation, which catches every subscriber up to the tip (the harness reports what fails
//! there as R12).
//!
//! [`SpvStore`] is generic over [`KvEngine`], so the same workload runs on SQLite under recorded
//! power-loss images (`dash-kv-sqlite`'s `sqlite_wal_prefix`).

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use crate::engine::{Cf, Commit, KvBatch, KvEngine, KvRead};
use crate::faulty::check::{InvariantId, Violation};
use crate::faulty::decide::Seeded;
use crate::faulty::engine::{FaultyEngine, Recovery};
use crate::faulty::harness::{Abort, Reference, Scenario, Sut, allowed_tips};
use crate::faulty::world::{Hash, Mark, Trace, World};
use crate::schema::{CfProfile, CfSpec, DbSchema};

#[cfg(test)]
mod tests;

/// The SPV store's schema.
pub static SPV_SCHEMA: DbSchema = DbSchema {
    name: "spv",
    version: 1,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("hdr", CfProfile::Ordered),
        CfSpec::new("active", CfProfile::Ordered),
        CfSpec::new("f", CfProfile::Ordered),
        CfSpec::new("m", CfProfile::Ordered),
    ],
};

/// The wallet's schema.
pub static WALLET_SCHEMA: DbSchema = DbSchema {
    name: "wallet",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("w", CfProfile::Ordered)],
};

const META: Cf = DbSchema::META;
const HDR: Cf = Cf::new(1);
const ACTIVE: Cf = Cf::new(2);
const TIP: &[u8] = b"tip";
const HEADERS_DIGEST: &[u8] = b"dg:h";
const BOUNDARIES: &[u8] = b"bnd";
const DONE: &[u8] = b"done";
const ABORT: &str = "SpvStore: a store call failed";

/// The anchor's hash.
pub const ANCHOR: u64 = 0x616e_6368_6f72_0000;

/// FNV-1a over `parts`, each prefixed by its length: header hashes, row values and digests.
pub fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in parts {
        for &b in (p.len() as u32).to_le_bytes().iter().chain(p.iter()) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

fn be64(b: &[u8]) -> u64 {
    u64::from_be_bytes(b.try_into().expect("8 bytes"))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b.try_into().expect("4 bytes"))
}

/// A step count as a trace hash (the flush marks' tips).
fn step_hash(step: u64) -> Hash {
    let mut h = [0u8; 32];
    h[..8].copy_from_slice(&step.to_le_bytes());
    h[31] = 0x73;
    h
}

/// The `active` rows of a chain from the anchor.
fn active_of(chain: &[u64]) -> BTreeMap<u32, u64> {
    chain.iter().enumerate().skip(1).map(|(h, &x)| (h as u32, x)).collect()
}

fn step_of(h: Hash) -> usize {
    u64::from_le_bytes(h[..8].try_into().expect("8 bytes")) as usize
}

/// A subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sub {
    /// Filter-like: a row per height, in the SPV store.
    F,
    /// MN-like: the newest block of each height residue mod 3, in the SPV store.
    M,
    /// Wallet-like: a row per height, in the wallet's database.
    W,
}

impl Sub {
    pub const ALL: [Sub; 3] = [Sub::F, Sub::M, Sub::W];

    fn index(self) -> usize {
        self as usize
    }

    fn letter(self) -> char {
        ['f', 'm', 'w'][self.index()]
    }

    fn cf(self) -> Cf {
        match self {
            Self::F => Cf::new(3),
            Self::M => Cf::new(4),
            Self::W => Cf::new(1),
        }
    }

    fn loc_key(self) -> Vec<u8> {
        format!("loc:{}", self.letter()).into_bytes()
    }

    fn digest_key(self) -> Vec<u8> {
        format!("dg:{}", self.letter()).into_bytes()
    }

    /// Its rows along `path`, the chain from the anchor to its locator.
    pub fn rows(self, path: &[u64]) -> Rows {
        let mut rows = Rows::new();
        for (h, hash) in path.iter().enumerate().skip(1) {
            let (key, value) = match self {
                Self::M => (vec![(h % 3) as u8], hash.to_be_bytes().to_vec()),
                _ => (
                    (h as u32).to_be_bytes().to_vec(),
                    fnv(&[&hash.to_be_bytes(), &[self.letter() as u8]]).to_be_bytes().to_vec(),
                ),
            };
            rows.insert(key, value);
        }
        rows
    }
}

/// A subscriber's rows.
pub type Rows = BTreeMap<Vec<u8>, Vec<u8>>;

/// The planted bugs (plan §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpvBug {
    /// A subscriber's rows and its locator in separate `WriteSet`s, rows first.
    B1,
    /// The header step split: `hdr` rows, then `active` and `meta`.
    B2,
    /// A header step committed after the subscriber step that follows it (commit-before-publish
    /// broken).
    B3,
    /// Recovery trusts a locator that names an unknown header.
    B4,
    /// A boundary committed Buffered.
    B5,
    /// Recovery's rewind resets the locator but not the rows.
    B6,
    /// A boundary that does not sync the wallet's WAL.
    B7,
}

impl FromStr for SpvBug {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "B1" => Self::B1,
            "B2" => Self::B2,
            "B3" => Self::B3,
            "B4" => Self::B4,
            "B5" => Self::B5,
            "B6" => Self::B6,
            "B7" => Self::B7,
            _ => return Err(()),
        })
    }
}

/// The engines' recovery and the planted bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpvConfig {
    pub recovery: Recovery,
    pub bug: Option<SpvBug>,
}

impl Default for SpvConfig {
    /// The SQLite store this models: prefix recovery, no bug.
    fn default() -> Self {
        Self {
            recovery: Recovery::Prefix,
            bug: None,
        }
    }
}

impl fmt::Display for SpvConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let engine = match self.recovery {
            Recovery::Prefix => "prefix",
            Recovery::Subset => "subset",
        };
        match self.bug {
            Some(b) => write!(f, "engine={engine} bug={b:?}"),
            None => write!(f, "engine={engine} bug=none"),
        }
    }
}

impl FromStr for SpvConfig {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let mut cfg = Self::default();
        for part in s.split_whitespace() {
            match part.split_once('=').ok_or(())? {
                ("engine", "prefix") => cfg.recovery = Recovery::Prefix,
                ("engine", "subset") => cfg.recovery = Recovery::Subset,
                ("bug", "none") => cfg.bug = None,
                ("bug", b) => cfg.bug = Some(b.parse()?),
                _ => return Err(()),
            }
        }
        Ok(cfg)
    }
}

/// An SPV store operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpvOp {
    /// A header step extending the active chain by `n` headers.
    Extend {
        n: u8,
    },
    /// A header step to a branch of `n` headers forking at height `at` (at most the tip's),
    /// tagged `tag`: the same fork, length and tag is the same branch.
    Reorg {
        at: u32,
        n: u8,
        tag: u8,
    },
    /// A subscriber step to the active block at height `upto` (at most the tip), from where
    /// its locator leaves the active chain.
    Sub {
        sub: Sub,
        upto: u32,
    },
    Boundary,
    /// R12's continuation: every subscriber to the tip, then a boundary.
    Continue,
}

impl fmt::Display for SpvOp {
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
            Self::Sub {
                sub,
                upto,
            } => write!(f, "{}{upto}", sub.letter()),
            Self::Boundary => f.write_str("b"),
            Self::Continue => f.write_str("C"),
        }
    }
}

impl FromStr for SpvOp {
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
            _ => Self::Sub {
                sub: *Sub::ALL.iter().find(|s| head.chars().eq([s.letter()])).ok_or(())?,
                upto: num(rest)?,
            },
        })
    }
}

/// What the two stores hold, in the miniature's terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpvState {
    /// `hdr`: hash → (height, prev).
    pub headers: BTreeMap<u64, (u32, u64)>,
    /// `active`: height → hash, from height 1.
    pub active: BTreeMap<u32, u64>,
    pub tip: Option<u64>,
    /// Each subscriber's rows and locator (`None`: never written, the anchor).
    pub rows: [Rows; 3],
    pub locs: [Option<u64>; 3],
    /// The digests as stored: the header step's, then each subscriber's.
    pub digests: [Option<u64>; 4],
    pub boundaries: u64,
    /// The continuation's mark: the tip it caught every subscriber up to.
    pub done: Option<u64>,
}

/// A store call failed (its name): the store stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed(pub &'static str);

impl SpvState {
    pub fn tip(&self) -> u64 {
        self.tip.unwrap_or(ANCHOR)
    }

    pub fn loc(&self, sub: Sub) -> u64 {
        self.locs[sub.index()].unwrap_or(ANCHOR)
    }

    /// Whether `hash` is a stored header or the anchor.
    pub fn knows(&self, hash: u64) -> bool {
        hash == ANCHOR || self.headers.contains_key(&hash)
    }

    /// The chain from the anchor to `hash`: `None` if a header on it is not stored.
    pub fn path(&self, hash: u64) -> Option<Vec<u64>> {
        let (mut path, mut at) = (vec![hash], hash);
        while at != ANCHOR {
            at = self.headers.get(&at)?.1;
            path.push(at);
            if path.len() > self.headers.len() + 1 {
                return None;
            }
        }
        path.reverse();
        Some(path)
    }

    /// The active chain as the tip names it.
    fn chain(&self) -> Vec<u64> {
        self.path(self.tip()).unwrap_or_else(|| vec![ANCHOR])
    }

    /// A digest of a part of the stores: 0 the header step's (headers, active, tip), then each
    /// subscriber's (rows and locator).
    pub fn digest(&self, part: usize) -> u64 {
        let mut d = 0;
        let mut add = |parts: &[&[u8]]| d ^= fnv(parts);
        if part == 0 {
            for (hash, (h, prev)) in &self.headers {
                add(&[b"h", &hash.to_be_bytes(), &h.to_be_bytes(), &prev.to_be_bytes()]);
            }
            for (h, hash) in &self.active {
                add(&[b"a", &h.to_be_bytes(), &hash.to_be_bytes()]);
            }
            add(&[b"t", &self.tip().to_be_bytes()]);
        } else {
            let sub = Sub::ALL[part - 1];
            for (k, v) in &self.rows[sub.index()] {
                add(&[b"r", k, v]);
            }
            add(&[b"l", &self.loc(sub).to_be_bytes()]);
        }
        d
    }

    /// The state after a header or subscriber step.
    pub fn step(&self, op: SpvOp) -> Self {
        match op {
            SpvOp::Extend {
                n,
            } => self.extend(u32::MAX, n, 0),
            SpvOp::Reorg {
                at,
                n,
                tag,
            } => self.extend(at, n, tag),
            SpvOp::Sub {
                sub,
                upto,
            } => self.advance(sub, upto),
            SpvOp::Boundary | SpvOp::Continue => self.clone(),
        }
    }

    fn extend(&self, at: u32, n: u8, tag: u8) -> Self {
        let mut s = self.clone();
        let mut chain = self.chain();
        chain.truncate(at.saturating_add(1) as usize);
        for _ in 0..n {
            let h = chain.len() as u32;
            let prev = *chain.last().expect("the anchor");
            let hash = fnv(&[&prev.to_be_bytes(), &h.to_be_bytes(), &[tag]]);
            s.headers.insert(hash, (h, prev));
            chain.push(hash);
        }
        s.active = active_of(&chain);
        s.tip = Some(*chain.last().expect("the anchor"));
        s.digests[0] = Some(s.digest(0));
        s
    }

    fn advance(&self, sub: Sub, upto: u32) -> Self {
        let chain = self.chain();
        // Where the locator leaves the active chain (an unknown one, only under B4: at the anchor).
        let path = self.path(self.loc(sub));
        let on = path.as_deref().unwrap_or(&[ANCHOR]);
        let fork = on.iter().zip(&chain).take_while(|(a, b)| a == b).count() - 1;
        let to = (upto as usize).min(chain.len() - 1).max(fork);
        if path.as_ref().is_some_and(|p| p.len() == fork + 1) && to == fork {
            return self.clone();
        }
        let mut s = self.clone();
        s.set_sub(sub, sub.rows(&chain[..=to]), chain[to]);
        s
    }

    /// A subscriber's rows and locator, and its digest.
    fn set_sub(&mut self, sub: Sub, rows: Rows, loc: u64) {
        self.rows[sub.index()] = rows;
        self.locs[sub.index()] = Some(loc);
        self.digests[sub.index() + 1] = Some(self.digest(sub.index() + 1));
    }

    /// Reads what the two stores hold.
    pub fn load(spv: &impl KvRead, wallet: &impl KvRead) -> Result<Self, Failed> {
        let mut st = Self {
            tip: meta(spv, TIP)?,
            boundaries: meta(spv, BOUNDARIES)?.unwrap_or(0),
            done: meta(spv, DONE)?,
            ..Self::default()
        };
        st.digests[0] = meta(spv, HEADERS_DIGEST)?;
        for (k, v) in scan(spv, HDR)? {
            st.headers.insert(be64(&k), (be32(&v[..4]), be64(&v[4..])));
        }
        for (k, v) in scan(spv, ACTIVE)? {
            st.active.insert(be32(&k), be64(&v));
        }
        for sub in Sub::ALL {
            let i = sub.index();
            (st.locs[i], st.digests[i + 1], st.rows[i]) = match sub {
                Sub::W => subscriber(wallet, sub)?,
                _ => subscriber(spv, sub)?,
            };
        }
        Ok(st)
    }
}

fn meta(db: &impl KvRead, key: &[u8]) -> Result<Option<u64>, Failed> {
    db.get(META, key).map(|v| v.map(|v| be64(&v))).map_err(|_| Failed("get"))
}

fn scan(db: &impl KvRead, cf: Cf) -> Result<Rows, Failed> {
    db.iter_prefix(cf, &[]).map(|r| r.map_err(|_| Failed("iter"))).collect()
}

/// A subscriber's locator, digest and rows.
fn subscriber(db: &impl KvRead, sub: Sub) -> Result<(Option<u64>, Option<u64>, Rows), Failed> {
    Ok((meta(db, &sub.loc_key())?, meta(db, &sub.digest_key())?, scan(db, sub.cf())?))
}

fn sync(db: &impl KvEngine) -> Result<(), Failed> {
    db.sync_wal().map_err(|_| Failed("sync_wal"))
}

/// Puts the entries of `new` that `old` lacks or holds otherwise, and deletes those it lacks.
fn diff<K: Ord, V: PartialEq>(
    b: &mut impl KvBatch,
    cf: Cf,
    old: &BTreeMap<K, V>,
    new: &BTreeMap<K, V>,
    encode: impl Fn(&K, &V) -> (Vec<u8>, Vec<u8>),
) {
    for (k, v) in old {
        if !new.contains_key(k) {
            b.delete(cf, &encode(k, v).0);
        }
    }
    for (k, v) in new {
        if old.get(k) != Some(v) {
            let (k, v) = encode(k, v);
            b.put(cf, &k, &v);
        }
    }
}

/// The store: the SPV database, the wallet's, and what they hold (Buffered writes included).
pub struct SpvStore<E: KvEngine> {
    spv: Arc<E>,
    wallet: Arc<E>,
    bug: Option<SpvBug>,
    st: SpvState,
    /// B3's header step, written after the subscriber step that follows it.
    deferred: Option<E::Batch>,
}

impl<E: KvEngine> SpvStore<E> {
    /// Opens and recovers the store.
    pub fn open(spv: Arc<E>, wallet: Arc<E>, bug: Option<SpvBug>) -> Result<Self, Failed> {
        // §9.5's order: what recovery reads is durable before it acts on it. No check here sees
        // the syncs go (nothing recovery reads is published before the next boundary).
        sync(&*spv)?;
        sync(&*wallet)?;
        let st = SpvState::load(&*spv, &*wallet)?;
        let mut s = Self {
            spv,
            wallet,
            bug,
            st,
            deferred: None,
        };
        for sub in Sub::ALL {
            if s.st.knows(s.st.loc(sub)) || s.bug == Some(SpvBug::B4) {
                continue;
            }
            let mut new = s.st.clone();
            let rows = if s.bug == Some(SpvBug::B6) {
                s.st.rows[sub.index()].clone()
            } else {
                Rows::new()
            };
            new.set_sub(sub, rows, ANCHOR);
            let mut b = s.db(sub).batch();
            s.put_rows(&mut b, sub, &new);
            s.put_loc(&mut b, sub, &new);
            s.commit(s.db(sub), b, Commit::Durable)?;
            s.st = new;
        }
        Ok(s)
    }

    /// What the stores hold, as this store wrote it.
    pub fn state(&self) -> &SpvState {
        &self.st
    }

    /// What the stores hold, read back.
    pub fn persisted(&self) -> Result<SpvState, Failed> {
        SpvState::load(&*self.spv, &*self.wallet)
    }

    fn db(&self, sub: Sub) -> &E {
        match sub {
            Sub::W => &self.wallet,
            _ => &self.spv,
        }
    }

    fn commit(&self, db: &E, b: E::Batch, commit: Commit) -> Result<(), Failed> {
        if b.is_empty() {
            return Ok(());
        }
        db.write(b, commit).map_err(|_| Failed("write"))
    }

    fn put_rows(&self, b: &mut E::Batch, sub: Sub, new: &SpvState) {
        let i = sub.index();
        diff(b, sub.cf(), &self.st.rows[i], &new.rows[i], |k, v| (k.clone(), v.clone()));
    }

    fn put_loc(&self, b: &mut E::Batch, sub: Sub, new: &SpvState) {
        if new.locs[sub.index()] != self.st.locs[sub.index()] || !b.is_empty() {
            b.put(META, &sub.loc_key(), &new.loc(sub).to_be_bytes());
            b.put(META, &sub.digest_key(), &new.digest(sub.index() + 1).to_be_bytes());
        }
    }

    fn put_active(&self, b: &mut E::Batch, new: &SpvState) {
        diff(b, ACTIVE, &self.st.active, &new.active, |h, x| {
            (h.to_be_bytes().to_vec(), x.to_be_bytes().to_vec())
        });
        if !b.is_empty() || new.tip != self.st.tip {
            b.put(META, TIP, &new.tip().to_be_bytes());
            b.put(META, HEADERS_DIGEST, &new.digest(0).to_be_bytes());
        }
    }

    fn flush_deferred(&mut self) -> Result<(), Failed> {
        match self.deferred.take() {
            Some(b) => self.commit(&self.spv, b, Commit::Buffered),
            None => Ok(()),
        }
    }

    /// A header or subscriber step.
    pub fn step(&mut self, op: SpvOp) -> Result<(), Failed> {
        let new = self.st.step(op);
        match op {
            SpvOp::Extend {
                ..
            }
            | SpvOp::Reorg {
                ..
            } => {
                self.flush_deferred()?;
                let mut b = self.spv.batch();
                diff(&mut b, HDR, &self.st.headers, &new.headers, |x, (h, prev)| {
                    (x.to_be_bytes().to_vec(), [&h.to_be_bytes()[..], &prev.to_be_bytes()].concat())
                });
                if self.bug == Some(SpvBug::B2) {
                    self.commit(&self.spv, b, Commit::Buffered)?;
                    b = self.spv.batch();
                }
                self.put_active(&mut b, &new);
                if self.bug == Some(SpvBug::B3) {
                    self.deferred = Some(b);
                } else {
                    self.commit(&self.spv, b, Commit::Buffered)?;
                }
            }
            SpvOp::Sub {
                sub,
                ..
            } => {
                let db = self.db(sub);
                let mut b = db.batch();
                self.put_rows(&mut b, sub, &new);
                if self.bug == Some(SpvBug::B1) {
                    self.commit(db, b, Commit::Buffered)?;
                    b = db.batch();
                }
                self.put_loc(&mut b, sub, &new);
                self.commit(db, b, Commit::Buffered)?;
                self.flush_deferred()?;
            }
            SpvOp::Boundary | SpvOp::Continue => unreachable!("not a step: {op}"),
        }
        self.st = new;
        Ok(())
    }

    /// A boundary: the SPV store's Durable commit, then the wallet's WAL sync.
    pub fn boundary(&mut self) -> Result<(), Failed> {
        self.flush_deferred()?;
        self.st.boundaries += 1;
        let commit = if self.bug == Some(SpvBug::B5) {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        self.mark(BOUNDARIES, self.st.boundaries, commit)?;
        if self.bug == Some(SpvBug::B7) {
            return Ok(());
        }
        sync(&*self.wallet)
    }

    /// R12's continuation: every subscriber to the tip, marked done, then a boundary.
    pub fn catch_up(&mut self) -> Result<(), Failed> {
        for sub in Sub::ALL {
            self.step(SpvOp::Sub {
                sub,
                upto: u32::MAX,
            })?;
        }
        self.st.done = Some(self.st.tip());
        self.mark(DONE, self.st.tip(), Commit::Durable)?;
        sync(&*self.wallet)
    }

    /// A boundary's mark: `key` in the SPV store's `meta`.
    fn mark(&self, key: &[u8], value: u64, commit: Commit) -> Result<(), Failed> {
        let mut b = self.spv.batch();
        b.put(META, key, &value.to_be_bytes());
        self.commit(&self.spv, b, commit)
    }
}

/// The reference: the state after each op, the first before any.
pub struct SpvRef {
    pub states: Vec<SpvState>,
}

impl SpvRef {
    /// Every header the workload accepts (none is ever deleted).
    fn all(&self) -> &SpvState {
        self.states.last().expect("the initial state")
    }
}

impl Reference for SpvRef {
    type Op = SpvOp;
    type Config = SpvConfig;

    fn new(_: &SpvConfig) -> Self {
        Self {
            states: vec![SpvState::default()],
        }
    }

    fn apply(&mut self, op: &SpvOp) {
        assert_ne!(*op, SpvOp::Continue, "the continuation is R12's");
        let next = self.all().step(*op);
        self.states.push(next);
    }
}

/// S1 and S4, on the stores as booted.
pub fn check_stores(st: &SpvState) -> Vec<Violation> {
    let mut v = Vec::new();
    // S1: the header graph is closed, and `active` is the chain from the tip to the anchor.
    for (hash, &(h, prev)) in &st.headers {
        let below = if prev == ANCHOR {
            Some(0)
        } else {
            st.headers.get(&prev).map(|p| p.0)
        };
        if below.map(|b| b + 1) != Some(h) {
            v.push(Violation::new(
                InvariantId::S1,
                format!("header {hash:x} at {h}: its parent is not stored below it"),
            ));
            break;
        }
    }
    match st.path(st.tip()) {
        None => v.push(Violation::new(InvariantId::S1, "the tip is not a stored header")),
        Some(p) => {
            let want = active_of(&p);
            if want != st.active {
                v.push(Violation::new(
                    InvariantId::S1,
                    format!("`active` is not the chain to the tip at {}", p.len() - 1),
                ));
            }
        }
    }
    // S4: no torn write set.
    let none = SpvState::default();
    for (part, name) in ["the header step's", "f's", "m's", "w's"].into_iter().enumerate() {
        if st.digests[part].unwrap_or_else(|| none.digest(part)) != st.digest(part) {
            v.push(Violation::new(InvariantId::S4, format!("{name} write set is torn")));
        }
    }
    v
}

/// S2, S3 and S5, on the stores as recovered: `acked` is the step the last acknowledged
/// boundary made durable.
pub fn check_recovered(st: &SpvState, refm: &SpvRef, acked: usize) -> Vec<Violation> {
    let mut v = Vec::new();
    // S2: a locator is a stored header or the anchor, and its subscriber's rows are its data
    // along the chain to it.
    for sub in Sub::ALL {
        let loc = st.loc(sub);
        let s = sub.letter();
        if !st.knows(loc) {
            v.push(Violation::new(
                InvariantId::S2,
                format!("{s}'s locator {loc:x} is not a stored header"),
            ));
        } else if let Some(path) = refm.all().path(loc) {
            if st.rows[sub.index()] != sub.rows(&path) {
                v.push(Violation::new(
                    InvariantId::S2,
                    format!("{s}'s rows are not its data to its locator at {}", path.len() - 1),
                ));
            }
        } else {
            v.push(Violation::new(
                InvariantId::S2,
                format!("{s}'s locator {loc:x} is no header the workload made"),
            ));
        }
    }
    let after = &refm.states[acked.min(refm.states.len() - 1)..];
    if let Some(done) = st.done {
        // S5: the continuation caught every subscriber up to the tip.
        for sub in Sub::ALL {
            if st.loc(sub) != done || st.tip() != done {
                v.push(Violation::new(
                    InvariantId::S5,
                    format!("after the continuation, {} is not at the tip", sub.letter()),
                ));
            }
        }
        if !after.iter().any(|r| r.tip() == st.tip()) {
            v.push(Violation::new(
                InvariantId::S3,
                format!("the tip is no state's from the acknowledged step {acked} on"),
            ));
        }
        return v;
    }
    // S3: each store recovered a state at or after the acknowledged step; the wallet's locator
    // may instead be the anchor, rewound from one the SPV store lost.
    let spv = |s: &SpvState| (s.tip(), s.loc(Sub::F), s.loc(Sub::M));
    if !after.iter().any(|r| spv(r) == spv(st)) {
        v.push(Violation::new(
            InvariantId::S3,
            format!("the SPV store is in no state from the acknowledged step {acked} on"),
        ));
    }
    let w = st.loc(Sub::W);
    if !after.iter().any(|r| r.loc(Sub::W) == w || (w == ANCHOR && !st.knows(r.loc(Sub::W)))) {
        v.push(Violation::new(
            InvariantId::S3,
            format!("w's locator is no state's from the acknowledged step {acked} on"),
        ));
    }
    v
}

/// The system under test.
pub struct SpvSut {
    world: World,
    store: SpvStore<FaultyEngine>,
    applied: u64,
    flushes: u64,
    acked: u64,
}

fn engines(world: &World, cfg: &SpvConfig) -> (Arc<FaultyEngine>, Arc<FaultyEngine>) {
    (
        world.engine_with("spv", &SPV_SCHEMA, cfg.recovery),
        world.engine_with("wallet", &WALLET_SCHEMA, cfg.recovery),
    )
}

fn fail(world: &World, f: Failed) -> Abort {
    world.mark(Mark::StoreFailed {
        op: f.0,
    });
    world.mark(Mark::Abort {
        message: ABORT.into(),
    });
    Abort::new(ABORT)
}

impl Sut for SpvSut {
    type Config = SpvConfig;
    type Op = SpvOp;
    type Ref = SpvRef;
    const NAME: &'static str = "spv";

    fn open(world: &World, cfg: &SpvConfig) -> Result<Self, Abort> {
        world.mark(Mark::RecoveryBegin);
        let (spv, wallet) = engines(world, cfg);
        let store = SpvStore::open(spv, wallet, cfg.bug).map_err(|f| fail(world, f))?;
        world.mark(Mark::RecoveryEnd);
        Ok(Self {
            world: world.clone(),
            store,
            applied: 0,
            flushes: 0,
            acked: 0,
        })
    }

    fn apply(&mut self, op: &SpvOp) -> Result<(), Abort> {
        self.applied += 1;
        let done = match op {
            SpvOp::Boundary => {
                self.flushes += 1;
                let flush = self.flushes;
                self.world.mark(Mark::FlushBegin {
                    flush,
                    old: Some(step_hash(self.acked)),
                    new: step_hash(self.applied),
                });
                let done = self.store.boundary();
                if done.is_ok() {
                    self.acked = self.applied;
                    self.world.mark(Mark::Durable {
                        flush,
                    });
                }
                done
            }
            SpvOp::Continue => self.store.catch_up(),
            op => self.store.step(*op),
        };
        done.map_err(|f| fail(&self.world, f))
    }

    fn check(&self, _: &World, refm: &SpvRef, trace: &Trace) -> Result<(), Vec<Violation>> {
        let st = self.store.persisted().map_err(|f| {
            vec![Violation::new(InvariantId::S2, format!("the stores do not read back: {f:?}"))]
        })?;
        let acked = allowed_tips(trace).0.map_or(0, step_of);
        let v = check_recovered(&st, refm, acked);
        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
        }
    }

    fn store_check(world: &World, cfg: &SpvConfig) -> Vec<Violation> {
        let (spv, wallet) = engines(world, cfg);
        match SpvState::load(&*spv, &*wallet) {
            Ok(st) => check_stores(&st),
            Err(f) => {
                vec![Violation::new(InvariantId::S1, format!("the stores do not read: {f:?}"))]
            }
        }
    }

    fn continue_after_recovery(&mut self) -> Option<SpvOp> {
        Some(SpvOp::Continue)
    }

    fn has_continuation() -> bool {
        true
    }

    fn sanctioned(_: &SpvConfig, message: &str) -> bool {
        message == ABORT
    }

    fn projection(&self) -> Option<Hash> {
        let st = self.store.state();
        let locs = Sub::ALL.map(|s| st.loc(s).to_be_bytes());
        Some(step_hash(fnv(&[&st.tip().to_be_bytes(), &locs[0], &locs[1], &locs[2]])))
    }
}

/// The exhaustive workload (here and on SQLite under recorded images): two boundaries; between
/// them dependent header steps, a reorg the subscribers follow, and wallet steps naming headers
/// whose step is still Buffered in the SPV store.
pub const WORKLOAD: &str = "h2 f2 m2 w2 b h2 f4 w4 h1 r1+3:1 m9 f9 w9 b h1 w9 f9";

/// A random workload on the correct store (the seeded sweep's).
pub fn random_scenario(rng: &mut Seeded) -> Scenario<SpvSut> {
    let mut st = SpvState::default();
    let mut ops = Vec::new();
    for _ in 0..4 + rng.below(9) {
        let tip = st.chain().len() as u64 - 1;
        let op = match rng.below(10) {
            0..=2 => SpvOp::Extend {
                n: 1 + rng.below(3) as u8,
            },
            3 if tip > 0 => SpvOp::Reorg {
                at: rng.below(tip) as u32,
                n: 1 + rng.below(3) as u8,
                tag: 1 + rng.below(3) as u8,
            },
            4..=7 => SpvOp::Sub {
                sub: Sub::ALL[rng.below(3) as usize],
                upto: rng.below(tip + 2) as u32,
            },
            _ => SpvOp::Boundary,
        };
        st = st.step(op);
        ops.push(op);
    }
    Scenario::new("spv-sweep", SpvConfig::default(), ops)
}
