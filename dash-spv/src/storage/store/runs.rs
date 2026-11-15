//! The active records' ids (SB14 D5): runs of consecutive ids that partition the active heights,
//! with disjoint id ranges, in memory and in SQLite's `hdr_runs` (key: the first height; value:
//! the first id, then the length when the run is closed).
//!
//! - **The base run (0):** with no rows and no `meta.next_id`, `[start, tip]` is one open run
//!   with first id 0 at `start`. It reaches SQLite with the first commit that writes runs.
//! - **Open (1):** the last run may be open: it extends to the tip, so a linear append writes
//!   nothing, and the next id is its implicit end.
//! - **Closing (2):** a lowering closes the open run at the in-memory tip and moves
//!   `meta.next_id` past every id issued.
//! - **New runs (3):** an append after a close starts an open run at `meta.next_id`; its row
//!   reaches SQLite before a file slot covers it (§3 Raise step 5).
//! - **Open's trim (4):** runs are trimmed to `[start, tip]`; a closed run stays closed.

/// A run: heights `h0..` hold ids `id0..`, for `len` heights or (open) up to the tip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub h0: u32,
    pub id0: u32,
    pub len: Option<u32>,
}

impl Run {
    /// The run's last height, with `tip` the active tip.
    fn last(&self, tip: u32) -> u32 {
        match self.len {
            Some(n) => self.h0 + n - 1,
            None => tip,
        }
    }

    /// The row: key the first height, value the first id and, when closed, the length.
    pub fn row(&self) -> ([u8; 4], Vec<u8>) {
        let mut v = self.id0.to_be_bytes().to_vec();
        if let Some(n) = self.len {
            v.extend_from_slice(&n.to_be_bytes());
        }
        (self.h0.to_be_bytes(), v)
    }

    pub fn from_row(k: &[u8], v: &[u8]) -> Result<Run, String> {
        let k: [u8; 4] = k.try_into().map_err(|_| format!("hdr_runs key of {} bytes", k.len()))?;
        let u32_at = |o: usize| u32::from_be_bytes(v[o..o + 4].try_into().expect("4 bytes"));
        let len = match v.len() {
            4 => None,
            8 if u32_at(4) > 0 => Some(u32_at(4)),
            n => return Err(format!("hdr_runs value of {n} bytes (or a zero length)")),
        };
        Ok(Run {
            h0: u32::from_be_bytes(k),
            id0: u32_at(0),
            len,
        })
    }
}

/// The runs over the active heights.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Runs {
    /// By first height.
    runs: Vec<Run>,
    /// `meta.next_id`: above every id issued outside the open run.
    pub next_id: u32,
    /// The rows differ from what SQLite holds (a new run, a close, the base run's first row).
    pub dirty: bool,
    /// No row was ever written: the base run (or nothing) stands in memory only.
    pub implicit: bool,
}

impl Runs {
    /// The base run over `[start, tip]`, or nothing on an empty store: what a store with no
    /// `hdr_runs` rows and no `meta.next_id` has (D5 (0)).
    pub fn base(range: Option<(u32, u32)>) -> Runs {
        Runs {
            runs: range
                .map(|(start, _)| Run {
                    h0: start,
                    id0: 0,
                    len: None,
                })
                .into_iter()
                .collect(),
            next_id: 0,
            dirty: false,
            implicit: true,
        }
    }

    /// The stored rows (any order) and `meta.next_id`, trimmed to `range` (Open step 4): a run
    /// wholly above the tip goes, a closed one is cut, an open one stays open. A height in the
    /// range with no run is corruption.
    pub fn stored(
        mut rows: Vec<Run>,
        next_id: u32,
        range: Option<(u32, u32)>,
    ) -> Result<Runs, String> {
        rows.sort_unstable_by_key(|r| r.h0);
        let mut out = Runs {
            runs: Vec::with_capacity(rows.len()),
            next_id,
            dirty: false,
            implicit: false,
        };
        let Some((start, tip)) = range else {
            out.dirty = !rows.is_empty();
            return Ok(out);
        };
        let mut want = start;
        for (i, r) in rows.iter().enumerate() {
            if r.len.is_none() && i + 1 != rows.len() {
                return Err(format!("hdr_runs: an open run at {} is not the last", r.h0));
            }
            let last = r.last(u32::MAX);
            if last < start || r.h0 > tip {
                out.dirty = true;
                continue;
            }
            if r.h0 > want {
                return Err(format!("hdr_runs: no run covers height {want}"));
            }
            if r.h0 < want {
                return Err(format!("hdr_runs: runs overlap at {}", r.h0));
            }
            let mut run = *r;
            if let Some(n) = run.len {
                if r.h0 + n - 1 > tip {
                    run.len = Some(tip - r.h0 + 1);
                    out.dirty = true;
                }
            }
            want = run.last(tip).saturating_add(1);
            out.runs.push(run);
        }
        if want <= tip {
            return Err(format!("hdr_runs: no run covers height {want}"));
        }
        if let Some(first) = out.runs.first() {
            if first.h0 != start {
                return Err(format!("hdr_runs start at {}, not {start}", first.h0));
            }
        }
        out.check_disjoint(tip)?;
        Ok(out)
    }

    fn check_disjoint(&self, tip: u32) -> Result<(), String> {
        let mut ranges: Vec<(u64, u64)> = self
            .runs
            .iter()
            .map(|r| {
                let n = u64::from(r.last(tip) - r.h0) + 1;
                (u64::from(r.id0), u64::from(r.id0) + n)
            })
            .collect();
        ranges.sort_unstable();
        for w in ranges.windows(2) {
            if w[1].0 < w[0].1 {
                return Err(format!("hdr_runs: id ranges overlap at {}", w[1].0));
            }
        }
        Ok(())
    }

    pub fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// The run holding height `h` (within the active range, checked by the caller).
    fn run_at(&self, h: u32) -> Option<&Run> {
        let i = self.runs.partition_point(|r| r.h0 <= h).checked_sub(1)?;
        Some(&self.runs[i])
    }

    /// The id at active height `h`, `tip` the active tip.
    pub fn id_at(&self, h: u32, tip: u32) -> Option<u32> {
        let r = self.run_at(h)?;
        (h <= r.last(tip)).then(|| r.id0 + (h - r.h0))
    }

    /// The active height of `id`, `tip` the active tip: the run whose id range holds it (the id
    /// ranges are disjoint, so at most one does).
    pub fn height_of(&self, id: u32, tip: u32) -> Option<u32> {
        self.runs.iter().find_map(|r| {
            let off = id.checked_sub(r.id0)?;
            (off <= r.last(tip) - r.h0).then(|| r.h0 + off)
        })
    }

    /// The open run, if the last run is open.
    pub fn open(&self) -> Option<&Run> {
        self.runs.last().filter(|r| r.len.is_none())
    }

    /// The id the next append at `tip + 1` takes: the open run's implicit next, else
    /// `meta.next_id` (where a new run starts).
    pub fn next_id(&self, tip: Option<u32>) -> u32 {
        match (self.open(), tip) {
            (Some(r), Some(t)) => r.id0 + (t - r.h0) + 1,
            _ => self.next_id,
        }
    }

    /// Every id issued is below this.
    pub fn issued(&self, tip: Option<u32>) -> u32 {
        self.next_id(tip).max(self.next_id)
    }

    /// An append at `h` (the tip + 1, or the start of an empty store): extends the open run, or
    /// starts one at `meta.next_id` (dirty: its row must reach SQLite before a file slot covers
    /// it). Returns the id.
    pub fn append(&mut self, h: u32, tip: Option<u32>) -> u32 {
        if let (Some(r), Some(t)) = (self.open(), tip) {
            debug_assert_eq!(t + 1, h);
            return r.id0 + (h - r.h0);
        }
        let id = self.next_id;
        self.runs.push(Run {
            h0: h,
            id0: id,
            len: None,
        });
        self.next_id = id + 1;
        // The base run (D5 (0)) needs no row while the store is implicit.
        self.dirty |= !self.implicit;
        id
    }

    /// Activates `id` at `h` (the tip + 1, or the start), an id not issued by extension (a
    /// re-activated record's, or one dash-chain gave): extends the last run when it is
    /// consecutive, else closes it and starts a closed one-height run.
    pub fn place(&mut self, h: u32, id: u32, tip: Option<u32>) {
        if tip.is_some() && self.next_id(tip) == id && self.open().is_some() {
            return;
        }
        self.close(tip);
        match self.runs.last_mut() {
            Some(r) if r.len.is_some_and(|n| r.h0 + n == h && r.id0 + n == id) => {
                r.len = r.len.map(|n| n + 1);
            }
            _ => self.runs.push(Run {
                h0: h,
                id0: id,
                len: Some(1),
            }),
        }
        self.next_id = self.next_id.max(id + 1);
        self.dirty = true;
    }

    /// The runs reached SQLite: nothing dirty, and no longer the implicit base run.
    pub fn written(&mut self) {
        self.dirty = false;
        self.implicit = false;
    }

    /// The runs `w`, taken earlier, reached SQLite: dirty exactly when memory differs from them
    /// (an append started a run meanwhile). Commits publish in commit order, so the last one
    /// decides, even when an older snapshot commits after a newer one.
    pub fn written_as(&mut self, w: &Runs) {
        self.implicit = false;
        self.dirty = !(self.runs == w.runs && self.next_id == w.next_id);
    }

    /// Whether a commit writing `meta` must carry the rows: they differ from SQLite's, or the
    /// base run has none yet over active records (D5 (0)).
    pub fn owed(&self) -> bool {
        self.dirty || (self.implicit && !self.runs.is_empty())
    }

    /// Closes the open run at `tip` (D5 (2)), moving `next_id` past it.
    pub fn close(&mut self, tip: Option<u32>) {
        let issued = self.issued(tip);
        if let (Some(r), Some(t)) = (self.runs.last_mut(), tip) {
            if r.len.is_none() {
                r.len = Some(t - r.h0 + 1);
                self.dirty = true;
            }
        }
        if issued != self.next_id {
            self.next_id = issued;
            self.dirty = true;
        }
    }

    /// The runs after the active chain is lowered to `tip` (`None`: no active records).
    pub fn trim(&mut self, tip: Option<u32>) {
        let Some(t) = tip else {
            if !self.runs.is_empty() {
                self.runs.clear();
                self.dirty = true;
            }
            return;
        };
        let before = self.runs.len();
        self.runs.retain(|r| r.h0 <= t);
        if self.runs.len() != before {
            self.dirty = true;
        }
        if let Some(r) = self.runs.last_mut() {
            if let Some(n) = r.len {
                if r.h0 + n - 1 > t {
                    r.len = Some(t - r.h0 + 1);
                    self.dirty = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_run_extends_and_a_lowering_closes_it() {
        let mut r = Runs::base(Some((100, 109)));
        assert_eq!(r.id_at(105, 109), Some(5));
        assert_eq!(r.height_of(9, 109), Some(109));
        assert_eq!(r.height_of(10, 109), None);
        assert_eq!(r.append(110, Some(109)), 10);
        assert!(!r.dirty, "a linear append writes nothing");
        // A truncation to 104 moves ids 5..=10 off: the run closes at 110, next_id 11.
        r.close(Some(110));
        r.trim(Some(104));
        assert_eq!(r.next_id, 11);
        assert_eq!(
            r.runs(),
            [Run {
                h0: 100,
                id0: 0,
                len: Some(5)
            }]
        );
        assert_eq!(r.append(105, Some(104)), 11, "a new run at next_id");
        assert_eq!(r.id_at(105, 105), Some(11));
        assert_eq!(r.height_of(5, 105), None, "a moved-off id is not active");
        assert!(r.dirty);
    }

    #[test]
    fn a_reactivated_id_gets_a_closed_run() {
        let mut r = Runs::base(Some((0, 9)));
        r.close(Some(9));
        r.trim(Some(4));
        r.place(5, 5, Some(4));
        r.place(6, 6, Some(5));
        // Consecutive with the closed run below: it grows, still closed.
        assert_eq!(
            r.runs(),
            [Run {
                h0: 0,
                id0: 0,
                len: Some(7)
            }]
        );
        assert_eq!(r.id_at(6, 6), Some(6));
        r.place(7, 8, Some(6));
        assert_eq!(
            r.runs()[1],
            Run {
                h0: 7,
                id0: 8,
                len: Some(1)
            }
        );
        assert_eq!(r.append(8, Some(7)), 10, "fresh ids from next_id");
        assert_eq!(r.runs().len(), 3);
    }

    #[test]
    fn stored_runs_trim_and_refuse_holes() {
        let rows = vec![
            Run {
                h0: 0,
                id0: 0,
                len: Some(5),
            },
            Run {
                h0: 5,
                id0: 20,
                len: Some(10),
            },
        ];
        let r = Runs::stored(rows.clone(), 30, Some((0, 8))).unwrap();
        assert_eq!(r.runs()[1].len, Some(4));
        assert!(r.dirty);
        assert!(Runs::stored(rows.clone(), 30, Some((0, 20))).is_err(), "a hole above");
        assert!(Runs::stored(rows[1..].to_vec(), 30, Some((0, 8))).is_err(), "a hole below");
        let overlap = vec![
            Run {
                h0: 0,
                id0: 0,
                len: Some(5),
            },
            Run {
                h0: 5,
                id0: 3,
                len: None,
            },
        ];
        assert!(Runs::stored(overlap, 30, Some((0, 8))).is_err(), "ids overlap");
        let (k, v) = rows[1].row();
        assert_eq!(Run::from_row(&k, &v).unwrap(), rows[1]);
    }

    /// M1: a raise's snapshot of the runs commits while an append starts a new run; the runs
    /// stay dirty, and an older snapshot committing after a newer one leaves them dirty again.
    #[test]
    fn written_as_tracks_the_last_commit() {
        let mut r = Runs::base(Some((0, 9)));
        r.close(Some(9));
        r.trim(Some(4));
        let older = r.clone();
        r.append(5, Some(4));
        let newer = r.clone();
        r.written_as(&older);
        assert!(r.dirty, "a run started after the snapshot");
        r.written_as(&newer);
        assert!(!r.dirty);
        r.written_as(&older);
        assert!(r.dirty, "SQLite holds the older runs");
        assert!(!r.implicit);
    }
}
