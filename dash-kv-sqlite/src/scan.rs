//! Prefix scans a page at a time, on one read view (SB0 §9.4): memory stays O(page), and the
//! scan sees one consistent view, as a RocksDB iterator does.

use std::collections::VecDeque;

use dash_kv::Result;
use rusqlite::Connection;

use crate::Tables;
use crate::error::engine_error;
use crate::pool::Pooled;

/// Rows fetched per page.
pub(crate) const PAGE: usize = 256;

/// The connection a scan reads on.
pub(crate) enum ScanConn<'a> {
    /// A live read's own connection, in a read transaction the scan opens; dropping the scan
    /// returns it (and its transaction ends).
    Owned(Pooled<'a>),
    /// A snapshot's connection, already pinned.
    Borrowed(&'a Connection),
}

impl ScanConn<'_> {
    fn conn(&self) -> &Connection {
        match self {
            Self::Owned(c) => c,
            Self::Borrowed(c) => c,
        }
    }
}

/// The successor of every key with `prefix`: the prefix with its last non-`0xff` byte
/// incremented and the rest dropped; `None` if every key from `prefix` on has it (it is empty or
/// all `0xff`).
pub(crate) fn upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let last = prefix.iter().rposition(|&b| b != 0xff)?;
    let mut bound = prefix[..=last].to_vec();
    bound[last] += 1;
    Some(bound)
}

pub(crate) struct Scan<'a> {
    conn: ScanConn<'a>,
    tables: &'a Tables,
    cf: usize,
    /// The next page starts at this key (inclusive at first, then after the last key returned).
    from: Vec<u8>,
    first: bool,
    until: Option<Vec<u8>>,
    page: VecDeque<(Vec<u8>, Vec<u8>)>,
    done: bool,
}

impl<'a> Scan<'a> {
    /// A scan of column family `cf` for keys with `prefix`. An `Owned` connection starts its read
    /// transaction here and reads the first page, which fixes the view (a deferred transaction
    /// takes it at its first read), so every page reads the view as of this call.
    pub(crate) fn new(
        conn: ScanConn<'a>,
        tables: &'a Tables,
        cf: usize,
        prefix: &[u8],
    ) -> Result<Self> {
        if let ScanConn::Owned(c) = &conn {
            crate::exec_cached(c, "BEGIN DEFERRED").map_err(|e| engine_error(c, e))?;
        }
        let mut scan = Self {
            conn,
            tables,
            cf,
            from: prefix.to_vec(),
            first: true,
            until: upper_bound(prefix),
            page: VecDeque::new(),
            done: false,
        };
        scan.fetch()?;
        Ok(scan)
    }

    fn fetch(&mut self) -> Result<()> {
        let sql = &self.tables.cfs[self.cf];
        let conn = self.conn.conn();
        let query = match (self.first, self.until.is_some()) {
            (true, true) => &sql.scan_from_until,
            (true, false) => &sql.scan_from,
            (false, true) => &sql.scan_after_until,
            (false, false) => &sql.scan_after,
        };
        let mut stmt = conn.prepare_cached(query).map_err(|e| engine_error(conn, e))?;
        let row = |r: &rusqlite::Row<'_>| Ok((r.get(0)?, r.get(1)?));
        let rows = match &self.until {
            Some(until) => stmt.query_map((&self.from, until), row),
            None => stmt.query_map((&self.from,), row),
        };
        let rows: Vec<(Vec<u8>, Vec<u8>)> = rows
            .and_then(|rows| rows.collect::<rusqlite::Result<_>>())
            .map_err(|e| engine_error(conn, e))?;
        self.done = rows.len() < PAGE;
        self.first = false;
        if let Some((last, _)) = rows.last() {
            self.from.clone_from(last);
        }
        self.page.extend(rows);
        Ok(())
    }
}

impl Iterator for Scan<'_> {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.page.is_empty()
            && !self.done
            && let Err(e) = self.fetch()
        {
            self.done = true;
            return Some(Err(e));
        }
        self.page.pop_front().map(Ok)
    }
}
