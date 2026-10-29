//! The height-indexed writes and reads of dash-spv's storage traits, on SB14's layout.
//!
//! - **Appends** at the tip + 1 (or anywhere on an empty store) go into the segment files under
//!   the lookup lock alone (D16), after D6's stored-header check. Their ids extend the open run
//!   (D5); nothing reaches SQLite.
//! - **An overwrite** (headers at or below the tip that differ from the active ones) lowers the
//!   tip to the first differing height (D7) and activates the new headers there, followed by the
//!   old records above them (re-activated with their ids): an overwrite below the top keeps the
//!   heights above it, as SB13 and v2 did.
//! - **A truncation** lowers the tip (D7): the records above it move off-chain, as every branch
//!   is kept (SB0 §4.4).
//! - A header already stored keeps its record and id at its height (re-activated if it is
//!   off-chain); at another height it is refused.

use std::collections::HashSet;

use dash_kv::{Commit, KvBatch, KvEngine};

use super::hdr::index::HEIGHT_CAP;
use super::hdr::io::HdrIo;
use super::hdr::{rec_hash, rec_header};
use super::lower::{Activate, Cap};
use super::schema::*;
use super::{
    id_height_in, kv, lock, refused, Bounds, ByHash, Error, HdrWriter, Point, Result, Store, END,
};

/// A header as the legacy traits store it: its (trusted) hash and its 80 bytes.
pub type RawHeader = ([u8; 32], [u8; 80]);

/// Core's `GetSkipHeight`.
// Core: chain.cpp:86-95 GetSkipHeight (and :83 InvertLowestOne)
pub(crate) fn skip_height(height: u32) -> u32 {
    let invert_lowest_one = |n: u32| n & n.wrapping_sub(1);
    if height < 2 {
        return 0;
    }
    match height & 1 {
        1 => invert_lowest_one(invert_lowest_one(height - 1)) + 1,
        _ => invert_lowest_one(height),
    }
}

/// The heights a legacy truncation keeps, under the legacy rule: nothing above the top is a
/// no-op, below the lowest stored height is refused.
fn truncation(bounds: Bounds, h: u32, what: &str) -> Result<Option<Bounds>> {
    match (bounds.lo, bounds.hi) {
        (Some(lo), Some(hi)) if h < hi => match h < lo {
            true => Err(refused(format!("{what}: truncate_above({h}) below start height ({lo})"))),
            false => Ok(Some(bounds.truncated(h))),
        },
        _ => Ok(None),
    }
}

impl<E: KvEngine, F: HdrIo> Store<E, F> {
    /// Stores `headers` at `start..`, active at those heights; storing must extend or overwrite
    /// the active range without a gap. The first store sets the start (the derived anchor).
    pub fn put_headers(&self, start: u32, headers: &[RawHeader]) -> Result<()> {
        // D18: lock-free, so the fast path keeps the lookup lock alone (D16).
        self.halted()?;
        let Some(last) = (headers.len() as u32).checked_sub(1) else {
            return Ok(());
        };
        let end = start
            .checked_add(last)
            .filter(|&e| e < HEIGHT_CAP)
            .ok_or_else(|| refused(format!("headers at {start} reach height 2^24 (D3)")))?;
        let mut own: HashSet<[u8; 32], ByHash> =
            HashSet::with_capacity_and_hasher(headers.len(), Default::default());
        for (i, (hash, _)) in headers.iter().enumerate() {
            if !own.insert(*hash) {
                return Err(refused(format!(
                    "header {:?} repeats at {} in one write",
                    dash_primitives::BlockHash::from(*hash),
                    start + i as u32
                )));
            }
        }
        self.reach(Point::AppendUnlocked);
        if self.try_append(start, headers)? {
            return Ok(());
        }
        let mut hw = self.hw();
        self.put_slow(&mut hw, start, end, headers)
    }

    /// The fast path (D16): an append at the tip + 1 (any height on an empty store) none of
    /// whose headers is stored, under the lookup lock alone. `false`: an overwrite, or a stored
    /// header to re-activate (the writer mutex's path).
    fn try_append(&self, start: u32, headers: &[RawHeader]) -> Result<bool> {
        let mut h = self.hdr();
        // Again under the lock: a write under it may have latched since `put_headers` checked.
        self.halted()?;
        match (h.start, h.tip) {
            (_, Some(t)) if start == t + 1 => {}
            (_, None) => {}
            (Some(lo), Some(t)) if start < lo || start > t + 1 => {
                return Err(refused(format!(
                    "headers at {start} leave a gap in the stored {lo}..={t}"
                )));
            }
            _ => return Ok(false),
        }
        // D6: a linked run on a linked store, onto a tip, needs only the `hdr_prev(tip)` probe.
        let linked_run = h.linked && h.tip.is_some() && h.links(headers);
        if linked_run && h.offchain > 0 {
            let db =
                self.db().ok_or_else(|| Error::Corrupt("off-chain records, no database".into()))?;
            let rows: Vec<_> = kv(db.iter_prefix(HDR_PREV, &h.tip_hash).collect())?;
            for (k, _) in rows {
                let id = trailing_id("hdr_prev", &k)?;
                if kv(db.get(HDR, &id_key(id)))?
                    .is_some_and(|v| v.get(..32) == Some(&headers[0].0[..]))
                {
                    return Ok(false);
                }
            }
        }
        if !linked_run {
            for (i, (hash, _)) in headers.iter().enumerate() {
                let at = start + i as u32;
                if let Some(stored) = h.find(hash)? {
                    return Err(stored_elsewhere(hash, stored, at));
                }
                if h.offchain > 0 {
                    let db = self
                        .db()
                        .ok_or_else(|| Error::Corrupt("off-chain records, no database".into()))?;
                    match id_height_in(db, hash)? {
                        Some((_, height)) if height != at => {
                            return Err(stored_elsewhere(hash, height, at));
                        }
                        Some(_) => return Ok(false),
                        None => {}
                    }
                }
            }
        }
        let mut tip = h.tip;
        // Only the first header can start a run; it is undone if no header lands.
        let saved = h.runs.open().is_none().then(|| h.runs.clone());
        for i in 0..headers.len() as u32 {
            h.runs.append(start + i, tip);
            tip = Some(start + i);
        }
        // A failure part way leaves memory ahead of the files: it latches (D18), so no slot
        // covers it and nothing lowers until a reopen. The runs cover exactly what landed, so
        // lookups keep working.
        let appended = h.append(start, headers);
        if appended.is_err() && h.tip.is_none_or(|t| t < start) {
            if let Some(runs) = saved {
                h.runs = runs;
            }
        }
        h.note(appended)?;
        Ok(true)
    }

    /// The writer mutex's path: an overwrite (D7), or an append that re-activates stored
    /// headers. Everything is checked before anything changes.
    fn put_slow(
        &self,
        hw: &mut HdrWriter<F>,
        start: u32,
        end: u32,
        headers: &[RawHeader],
    ) -> Result<()> {
        self.halted()?;
        let (lo, tip) = {
            let h = self.hdr();
            (h.start, h.tip)
        };
        if let (Some(lo), Some(t)) = (lo, tip) {
            if start < lo || start > t + 1 {
                return Err(refused(format!(
                    "headers at {start} leave a gap in the stored {lo}..={t}"
                )));
            }
        }
        // The first height whose header differs from the active one.
        let mut f = None;
        if let Some(t) = tip {
            let mut h = self.hdr();
            for at in start..=end.min(t) {
                if h.hash_at(at)? != headers[(at - start) as usize].0 {
                    f = Some(at);
                    break;
                }
            }
        }
        let (from, lowered) = match (f, tip) {
            (Some(f), _) => (f, true),
            (None, Some(t)) if end <= t => return Ok(()),
            (None, Some(t)) => (t + 1, false),
            (None, None) => (start, false),
        };
        let db = self.db();
        let mut list = Vec::with_capacity((end - from + 1) as usize);
        for at in from..=end {
            let (hash, header) = headers[(at - start) as usize];
            let active = self.hdr().find(&hash)?;
            let a = match active {
                Some(s) if s != at => return Err(stored_elsewhere(&hash, s, at)),
                Some(s) => Activate {
                    hash,
                    header,
                    id: self.active_id(s).map(|id| id.0),
                    stored: true,
                    row: None,
                },
                None => match db.map(|db| id_height_in(db, &hash)).transpose()?.flatten() {
                    Some((_, height)) if height != at => {
                        return Err(stored_elsewhere(&hash, height, at));
                    }
                    Some((id, _)) => Activate {
                        hash,
                        header,
                        id: Some(id.0),
                        stored: true,
                        row: None,
                    },
                    None => Activate {
                        hash,
                        header,
                        id: None,
                        stored: false,
                        row: None,
                    },
                },
            };
            list.push(a);
        }
        // An overwrite below the top keeps the old records above it, re-activated.
        if let (true, Some(t)) = (lowered, tip) {
            for at in end + 1..=t {
                let (r, id) = {
                    let mut h = self.hdr();
                    (h.rec(at)?, h.runs.id_at(at, t).expect("active"))
                };
                let row = match lock(&self.overrides).contains(&id) {
                    true => self.stored_row(dash_chain::BlockId(id))?,
                    false => None,
                };
                list.push(Activate {
                    hash: rec_hash(&r),
                    header: rec_header(&r),
                    id: Some(id),
                    stored: true,
                    row,
                });
            }
        }
        if lowered {
            self.lower(hw, from)?;
        }
        let a = self.activate(hw, from, &list)?;
        self.commit_activation(hw, a, Commit::Buffered, |_| {})
    }

    /// Lowers the tip to `h` (D7): the records above it move off-chain. Nothing above the tip is
    /// a no-op; below the start is refused.
    pub fn truncate_headers(&self, h: u32) -> Result<()> {
        let mut hw = self.hw();
        self.halted()?;
        let active = self.view().active;
        if truncation(active, h, "headers")?.is_none() {
            return Ok(());
        }
        self.lower(&mut hw, h + 1)
    }

    /// The active headers at `range`, which must lie within the stored heights.
    pub fn headers(&self, range: std::ops::Range<u32>) -> Result<Vec<RawHeader>> {
        if range.is_empty() {
            return Ok(Vec::new());
        }
        let mut h = self.hdr();
        if h.start.is_none_or(|lo| range.start < lo) || h.tip.is_none_or(|hi| range.end - 1 > hi) {
            return Err(refused(format!(
                "headers {range:?} outside the stored {:?}..={:?}",
                h.start, h.tip
            )));
        }
        Ok(h.recs(range)?.iter().map(|r| (rec_hash(r), rec_header(r))).collect())
    }

    /// The height of `hash` when it is on the active chain.
    pub fn active_height_of(&self, hash: &[u8; 32]) -> Result<Option<u32>> {
        Ok(self.hdr().find(hash)?)
    }

    /// The active hash at `h`.
    pub fn active_hash(&self, h: u32) -> Result<Option<[u8; 32]>> {
        let mut hd = self.hdr();
        match hd.holds(h) {
            true => Ok(Some(hd.hash_at(h)?)),
            false => Ok(None),
        }
    }

    /// The locator a subscriber reaching `top` gets: the highest active header at or below
    /// it and `cap`, if any. Recovery drops the rows above it (D35).
    pub(super) fn locator_at(&self, top: Option<u32>, cap: Cap) -> Result<Option<[u8; 36]>> {
        let mut h = self.hdr();
        let (Some(top), Some(lo), Some(hi)) = (top, h.start, h.tip) else {
            return Ok(None);
        };
        let Some(at) = cap.clamp(top.min(hi)) else {
            return Ok(None);
        };
        if at < lo {
            return Ok(None);
        }
        Ok(Some(encode_locator(&h.hash_at(at)?, at)))
    }

    /// The height a locator names.
    pub(super) fn locator_height(locator: Option<&[u8; 36]>) -> Option<u32> {
        locator.and_then(|v| decode_locator(v).ok()).map(|(_, h)| h)
    }

    pub(super) fn put_locator(b: &mut E::Batch, s: Subscriber, locator: Option<[u8; 36]>) {
        match locator {
            Some(v) => b.put(LOCATORS, s.locator_key(), &v),
            None => b.delete(LOCATORS, s.locator_key()),
        }
    }

    /// Stores `values` at `start..` in `s`'s table, with its locator, in one Buffered
    /// `WriteSet` (SB0 §9.5). The locator names the highest active header at or below the
    /// subscriber's top ([`locator_at`](Self::locator_at)), at or below the committed tip (a raise
    /// rides the commit otherwise, §3); with none (checkpoint sync's filter header below the
    /// anchor), the previous locator stays, so recovery still rewinds rows whose branch a reorg
    /// leaves. The bounds and the locator are read under the lease (D16 (3)).
    pub fn put_sub(&self, s: Subscriber, start: u32, values: &[&[u8]]) -> Result<()> {
        self.halted()?;
        let Some(last) = (values.len() as u32).checked_sub(1) else {
            return Ok(());
        };
        let end = start.checked_add(last).ok_or_else(|| refused("heights overflow"))?;
        self.commit_naming(
            Commit::Buffered,
            |st, db, cap| {
                let bounds = st.sub_bounds(s).extended(start, end);
                let locator = st.locator_at(bounds.hi, cap)?;
                let mut b = db.batch();
                for (h, v) in (start..=end).zip(values) {
                    b.put(s.cf(), &be32(h), v);
                }
                if let Some(locator) = locator {
                    b.put(LOCATORS, s.locator_key(), &locator);
                }
                Ok((b, Self::locator_height(locator.as_ref()), bounds))
            },
            |st, bounds| st.rows_write().subs[s as usize] = bounds,
        )
    }

    /// Removes `s`'s rows above `h`, under the legacy rule, with its locator. The bounds and the
    /// locator are read again under the lease (D16 (3)): a lowering may have rewound them.
    pub fn truncate_sub(&self, s: Subscriber, h: u32) -> Result<()> {
        self.halted()?;
        if truncation(self.sub_bounds(s), h, s.name())?.is_none() {
            return Ok(());
        }
        self.commit_naming(
            Commit::Buffered,
            |st, db, cap| {
                let mut b = db.batch();
                let Some(bounds) = truncation(st.sub_bounds(s), h, s.name())? else {
                    return Ok((b, None, None));
                };
                let locator = st.locator_at(bounds.hi, cap)?;
                b.delete_range(s.cf(), &be32(h + 1), &END);
                Self::put_locator(&mut b, s, locator);
                Ok((b, Self::locator_height(locator.as_ref()), Some(bounds)))
            },
            |st, bounds| {
                if let Some(bounds) = bounds {
                    st.rows_write().subs[s as usize] = bounds;
                }
            },
        )
    }

    /// Removes every row of `s` and its locator.
    pub fn clear_sub(&self, s: Subscriber, durability: Commit) -> Result<()> {
        self.halted()?;
        if self.db().is_none() {
            return Ok(());
        }
        self.commit_naming(
            durability,
            |_, db, _| {
                let mut b = db.batch();
                b.delete_range(s.cf(), &[], &END);
                b.delete(LOCATORS, s.locator_key());
                Ok((b, None, ()))
            },
            |st, ()| st.rows_write().subs[s as usize] = Bounds::default(),
        )
    }

    /// `s`'s values at `range`, which must lie within its stored heights; a missing row there
    /// (a sparse table's hole) is refused.
    pub fn sub_range(&self, s: Subscriber, range: std::ops::Range<u32>) -> Result<Vec<Vec<u8>>> {
        if range.is_empty() {
            return Ok(Vec::new());
        }
        // Held across the read: a writer cannot truncate the range between the check and it.
        let _fence = self.fence_read();
        let bounds = self.view().sub(s);
        if bounds.lo.is_none_or(|lo| range.start < lo)
            || bounds.hi.is_none_or(|hi| range.end - 1 > hi)
        {
            return Err(refused(format!("{} {range:?} outside the stored {bounds:?}", s.name())));
        }
        let db = self.db().ok_or_else(|| Error::Corrupt("subscriber rows, no database".into()))?;
        let keys: Vec<[u8; 4]> = range.clone().map(be32).collect();
        let refs: Vec<&[u8]> = keys.iter().map(|k| &k[..]).collect();
        kv(db.multi_get(s.cf(), &refs))?
            .into_iter()
            .zip(range)
            .map(|(v, h)| v.ok_or_else(|| refused(format!("{} has no row at {h}", s.name()))))
            .collect()
    }

    /// A row of `cf`, or `None` with no database (D4).
    fn get_row(&self, cf: dash_kv::Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.db() {
            Some(db) => kv(db.get(cf, key)),
            None => Ok(None),
        }
    }

    /// `s`'s value at `h`.
    pub fn sub_get(&self, s: Subscriber, h: u32) -> Result<Option<Vec<u8>>> {
        self.get_row(s.cf(), &be32(h))
    }

    /// `s`'s stored locator: the hash and height its data reaches.
    pub fn locator(&self, s: Subscriber) -> Result<Option<([u8; 32], u32)>> {
        match self.get_row(LOCATORS, s.locator_key())? {
            Some(v) => Ok(Some(decode_locator(&v)?)),
            None => Ok(None),
        }
    }

    fn legacy_key(key: &str) -> Vec<u8> {
        [meta_key::LEGACY, key.as_bytes()].concat()
    }

    /// Stores a legacy metadata value, in its own Buffered `WriteSet`.
    pub fn put_metadata(&self, key: &str, value: &[u8]) -> Result<()> {
        self.commit_naming(
            Commit::Buffered,
            |_, db, _| {
                let mut b = db.batch();
                b.put(META, &Self::legacy_key(key), value);
                Ok((b, None, ()))
            },
            |_, ()| {},
        )
    }

    pub fn metadata(&self, key: &str) -> Result<Option<Vec<u8>>> {
        self.get_row(META, &Self::legacy_key(key))
    }

    /// Stores an old-path masternode message (SB13 D6: until SB15's verified rows). It names
    /// `height` (clamped to the tip, under the lease): the committed tip covers it, or a raise
    /// rides the commit (§3).
    pub fn put_mn_message(&self, height: u32, kind: u8, bytes: &[u8]) -> Result<()> {
        self.commit_naming(
            Commit::Buffered,
            |st, db, cap| {
                let names = {
                    let h = st.hdr();
                    h.tip
                        .and_then(|t| cap.clamp(height.min(t)))
                        .filter(|&n| h.start.is_some_and(|s| n >= s))
                };
                let mut b = db.batch();
                b.put(MN_LOG, &mn_log_key(height, kind), bytes);
                Ok((b, names, ()))
            },
            |_, ()| {},
        )
    }

    pub fn mn_message(&self, height: u32, kind: u8) -> Result<Option<Vec<u8>>> {
        self.get_row(MN_LOG, &mn_log_key(height, kind))
    }

    /// Every stored masternode message's `(height, kind)`, in key order.
    pub fn mn_messages(&self) -> Result<Vec<(u32, u8)>> {
        let Some(db) = self.db() else {
            return Ok(Vec::new());
        };
        let keys: Vec<_> = kv(db.iter_prefix(MN_LOG, &[]).collect())?;
        keys.into_iter()
            .map(|(k, _)| match k.len() {
                5 => Ok((read_u32("mn_log", &k[..4])?, k[4])),
                n => Err(Error::Corrupt(format!("mn_log key of {n} bytes"))),
            })
            .collect()
    }
}

/// The refusal of a header stored at another height.
fn stored_elsewhere(hash: &[u8; 32], stored: u32, at: u32) -> Error {
    refused(format!(
        "header {:?} is stored at {stored}, not {at}",
        dash_primitives::BlockHash::from(*hash)
    ))
}

/// `(height, kind)`: an `mn_log` key.
fn mn_log_key(height: u32, kind: u8) -> [u8; 5] {
    let mut k = [0; 5];
    k[..4].copy_from_slice(&height.to_be_bytes());
    k[4] = kind;
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_heights_match_core() {
        // Core: chain.cpp:86-95; the same table as dash-chain's walk.rs test.
        let got: Vec<u32> = (0..12).map(skip_height).collect();
        assert_eq!(got, [0, 0, 0, 1, 0, 1, 4, 1, 0, 1, 8, 1]);
    }
}
