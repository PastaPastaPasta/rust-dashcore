//! The SPV store's schema, storage version 3 (SB0 §9.5, SB5 §7), and the byte layout of every
//! key and value. Keys are big-endian, so key order is numeric order.
//!
//! The logical tables are SB5 §7's, as `dash-kv` column families, which `dash-kv-sqlite` stores
//! as `WITHOUT ROWID` tables (SB0 §9.4):
//!
//! | Column family | Key | Value |
//! |---|---|---|
//! | `meta` | name | `anchor` hash ‖ height, `anchor_state` u8, `next_id`, `tip`, `best_header` u32, `sync_seq` u64, `cl_enabled_observed` u8; the legacy metadata under `md:` |
//! | `hdr` | id | the `hdr` row: hash ‖ height ‖ header ‖ skip ‖ status ‖ chain work ‖ `time_max` ‖ kind |
//! | `hdr_hash` | hash | id ‖ height (the `UNIQUE` hash of `hdr`, SB5 §7's primary key, covering the height) |
//! | `hdr_h` | height ‖ id | — (every branch by height) |
//! | `hdr_prev` | prev hash ‖ id | — (`children`, DEC-123 Q7) |
//! | `hdr_leaf` | id | — |
//! | `active` | height | id (unused since SB14: the active chain is in the header files) |
//! | `hdr_runs` | first height | first id ‖ length (open: no length): SB14 D5's id runs |
//! | `locks` | `best`, `floor` | `best`: height ‖ hash; `floor`: id ‖ height ‖ hash |
//! | `cfhdr`, `cfilter`, `blocks` | height | the filter header, the filter, the block |
//! | `locators` | subscriber | hash ‖ height |
//! | `mn_lists`, `mn_quorums`, `mn_snapshots` | — | SB15's verified MN state (O3); empty until then |
//! | `mn_log` | height ‖ kind | the old MN path's stored message (until SB15) |
//! | `peers`, `bans` | — | SB16's `NetStore`; empty until then |
//!
//! `hdr` is keyed by id and `hdr_hash` maps hash → id: SB5 §7 names the hash the primary key and
//! the id `UNIQUE`; the two are the same pair of unique indexes, and the id-keyed one serves the
//! height-ordered reads (`from_height`, `active` ranges) with one point read per record. The
//! hash index also holds the record's height, which never changes, so a hash resolves to its
//! active height without reading the record (SB13 D43).

use dash_chain::{AnchorState, BlockId, ClPersisted, HeaderRow};
use dash_kv::{Cf, CfProfile, CfSpec, DbSchema};

/// The store's schema: `meta.schema` is 3, dash-chain's `CHAIN_SCHEMA` (SB5 §7).
pub static SPV_SCHEMA: DbSchema = DbSchema {
    name: "spv",
    version: dash_chain::CHAIN_SCHEMA,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("hdr", CfProfile::Ordered),
        CfSpec::new("hdr_hash", CfProfile::PointLookup),
        CfSpec::new("hdr_h", CfProfile::Ordered),
        CfSpec::new("hdr_prev", CfProfile::Ordered),
        CfSpec::new("hdr_leaf", CfProfile::Ordered),
        CfSpec::new("active", CfProfile::Ordered),
        CfSpec::new("locks", CfProfile::Ordered),
        CfSpec::new("cfhdr", CfProfile::Ordered),
        CfSpec::new("cfilter", CfProfile::Ordered),
        CfSpec::new("blocks", CfProfile::Ordered),
        CfSpec::new("locators", CfProfile::Ordered),
        CfSpec::new("mn_lists", CfProfile::Ordered),
        CfSpec::new("mn_quorums", CfProfile::Ordered),
        CfSpec::new("mn_snapshots", CfProfile::Ordered),
        CfSpec::new("mn_log", CfProfile::Ordered),
        CfSpec::new("peers", CfProfile::Ordered),
        CfSpec::new("bans", CfProfile::Ordered),
        CfSpec::new("hdr_runs", CfProfile::Ordered),
    ],
};

pub(crate) const META: Cf = DbSchema::META;
pub(crate) const HDR: Cf = Cf::new(1);
pub(crate) const HDR_HASH: Cf = Cf::new(2);
pub(crate) const HDR_H: Cf = Cf::new(3);
pub(crate) const HDR_PREV: Cf = Cf::new(4);
pub(crate) const HDR_LEAF: Cf = Cf::new(5);
pub(crate) const ACTIVE: Cf = Cf::new(6);
pub(crate) const LOCKS: Cf = Cf::new(7);
pub(crate) const CFHDR: Cf = Cf::new(8);
pub(crate) const CFILTER: Cf = Cf::new(9);
pub(crate) const BLOCKS: Cf = Cf::new(10);
pub(crate) const LOCATORS: Cf = Cf::new(11);
pub(crate) const MN_LOG: Cf = Cf::new(15);
/// SB14 D5: the active records' id runs, by first height.
pub(crate) const HDR_RUNS: Cf = Cf::new(18);

/// The header tables a Candidate fallback clears (SB0 §9.5).
pub(crate) const HEADER_CFS: [Cf; 8] =
    [HDR, HDR_HASH, HDR_H, HDR_PREV, HDR_LEAF, ACTIVE, LOCKS, HDR_RUNS];

pub(crate) mod meta_key {
    pub const ANCHOR: &[u8] = b"anchor";
    pub const ANCHOR_STATE: &[u8] = b"anchor_state";
    pub const NEXT_ID: &[u8] = b"next_id";
    pub const TIP: &[u8] = b"tip";
    pub const BEST_HEADER: &[u8] = b"best_header";
    pub const SYNC_SEQ: &[u8] = b"sync_seq";
    pub const CL_ENABLED_OBSERVED: &[u8] = b"cl_enabled_observed";
    /// The legacy `MetadataStorage` keys, until SB14 gives each a home.
    pub const LEGACY: &[u8] = b"md:";
    /// SB14 D2: the SQLite slot of the committed tip.
    pub const HDR_MARKER: &[u8] = b"hdr_marker";
}

pub(crate) const LOCK_BEST: &[u8] = b"best";
pub(crate) const LOCK_FLOOR: &[u8] = b"floor";

/// A stored row that does not decode: the store is corrupt.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{cf}: {what}")]
pub struct BadRow {
    pub cf: &'static str,
    pub what: String,
}

fn bad(cf: &'static str, what: impl Into<String>) -> BadRow {
    BadRow {
        cf,
        what: what.into(),
    }
}

pub(crate) fn be32(n: u32) -> [u8; 4] {
    n.to_be_bytes()
}

pub(crate) fn id_key(id: BlockId) -> [u8; 4] {
    id.0.to_be_bytes()
}

/// A 4-byte big-endian integer.
pub(crate) fn read_u32(cf: &'static str, b: &[u8]) -> Result<u32, BadRow> {
    Ok(u32::from_be_bytes(b.try_into().map_err(|_| bad(cf, format!("{} bytes, not 4", b.len())))?))
}

fn read_u64(cf: &'static str, b: &[u8]) -> Result<u64, BadRow> {
    Ok(u64::from_be_bytes(b.try_into().map_err(|_| bad(cf, format!("{} bytes, not 8", b.len())))?))
}

pub(crate) fn read_hash(cf: &'static str, b: &[u8]) -> Result<[u8; 32], BadRow> {
    b.try_into().map_err(|_| bad(cf, format!("{} bytes, not a 32-byte hash", b.len())))
}

/// `id ‖ height`: an `hdr_hash` value.
pub(crate) fn hash_value(id: BlockId, height: u32) -> [u8; 8] {
    let mut v = [0; 8];
    v[..4].copy_from_slice(&id_key(id));
    v[4..].copy_from_slice(&height.to_be_bytes());
    v
}

/// An `hdr_hash` value: the id and the height of the record.
pub(crate) fn read_hash_value(v: &[u8]) -> Result<(BlockId, u32), BadRow> {
    let v: &[u8; 8] =
        v.try_into().map_err(|_| bad("hdr_hash", format!("{} bytes, not 8", v.len())))?;
    let (id, height) = v.split_at(4);
    Ok((BlockId(read_u32("hdr_hash", id)?), read_u32("hdr_hash", height)?))
}

/// `(height, id)`: an `hdr_h` key.
pub(crate) fn height_id_key(height: u32, id: BlockId) -> [u8; 8] {
    let mut k = [0; 8];
    k[..4].copy_from_slice(&height.to_be_bytes());
    k[4..].copy_from_slice(&id.0.to_be_bytes());
    k
}

/// `(prev, id)`: an `hdr_prev` key.
pub(crate) fn prev_id_key(prev: &[u8; 32], id: BlockId) -> [u8; 36] {
    let mut k = [0; 36];
    k[..32].copy_from_slice(prev);
    k[32..].copy_from_slice(&id.0.to_be_bytes());
    k
}

/// The last four bytes of a key as an id (`hdr_h`, `hdr_prev`).
pub(crate) fn trailing_id(cf: &'static str, k: &[u8]) -> Result<BlockId, BadRow> {
    match k.len().checked_sub(4) {
        Some(at) => Ok(BlockId(read_u32(cf, &k[at..])?)),
        None => Err(bad(cf, "key shorter than an id")),
    }
}

/// The `prev_hash` of an `hdr` row: header bytes 4..36, internal byte order (SB5-S1 n10).
pub(crate) fn prev_of(row: &HeaderRow) -> [u8; 32] {
    row.header[4..36].try_into().expect("32 bytes")
}

const ROW_LEN: usize = 32 + 4 + 80 + 1 + 4 + 4 + 32 + 4 + 1;

/// An `hdr` value.
pub(crate) fn encode_row(row: &HeaderRow) -> Vec<u8> {
    let mut v = Vec::with_capacity(ROW_LEN);
    v.extend_from_slice(&row.hash);
    v.extend_from_slice(&row.height.to_be_bytes());
    v.extend_from_slice(&row.header);
    match row.skip {
        Some(s) => {
            v.push(1);
            v.extend_from_slice(&s.to_be_bytes());
        }
        None => v.extend_from_slice(&[0; 5]),
    }
    v.extend_from_slice(&row.status.to_be_bytes());
    v.extend_from_slice(&row.chain_work);
    v.extend_from_slice(&row.time_max.to_be_bytes());
    v.push(row.kind);
    v
}

/// The `hdr` row stored under `id`.
pub(crate) fn decode_row(id: u32, v: &[u8]) -> Result<HeaderRow, BadRow> {
    if v.len() != ROW_LEN {
        return Err(bad("hdr", format!("row {id}: {} bytes, not {ROW_LEN}", v.len())));
    }
    let take = |at: usize, n: usize| &v[at..at + n];
    let skip = match v[116] {
        0 => None,
        1 => Some(u32::from_be_bytes(take(117, 4).try_into().expect("4"))),
        f => return Err(bad("hdr", format!("row {id}: skip flag {f}"))),
    };
    Ok(HeaderRow {
        hash: take(0, 32).try_into().expect("32"),
        id,
        height: u32::from_be_bytes(take(32, 4).try_into().expect("4")),
        header: take(36, 80).try_into().expect("80"),
        skip,
        status: u32::from_be_bytes(take(121, 4).try_into().expect("4")),
        chain_work: take(125, 32).try_into().expect("32"),
        time_max: u32::from_be_bytes(take(157, 4).try_into().expect("4")),
        kind: v[161],
    })
}

/// The `meta` rows of a [`dash_chain::ChainMeta`] but the schema record (dash-kv's), and the
/// `locks` rows of its ChainLock state.
pub(crate) fn meta_rows(m: &dash_chain::ChainMeta) -> [(&'static [u8], Vec<u8>); 7] {
    let mut anchor = m.anchor.0.as_bytes().to_vec();
    anchor.extend_from_slice(&m.anchor.1.to_be_bytes());
    [
        (meta_key::ANCHOR, anchor),
        (meta_key::ANCHOR_STATE, vec![m.anchor_state as u8]),
        (meta_key::NEXT_ID, id_key(m.next_id).to_vec()),
        (meta_key::TIP, id_key(m.tip).to_vec()),
        (meta_key::BEST_HEADER, id_key(m.best_header).to_vec()),
        (meta_key::SYNC_SEQ, m.sync_seq.to_be_bytes().to_vec()),
        (meta_key::CL_ENABLED_OBSERVED, vec![u8::from(m.cl.enabled_observed)]),
    ]
}

/// The `locks` rows of a ChainLock state: `None` deletes the row.
pub(crate) fn lock_rows(
    cl: &ClPersisted,
    floor: Option<(u32, [u8; 32])>,
) -> [(&'static [u8], Option<Vec<u8>>); 2] {
    let best = cl.best.map(|b| [&b.height.to_be_bytes()[..], b.hash.as_bytes()].concat());
    let floor = cl
        .floor
        .zip(floor)
        .map(|(id, (h, hash))| [&id_key(id)[..], &h.to_be_bytes()[..], &hash[..]].concat());
    [(LOCK_BEST, best), (LOCK_FLOOR, floor)]
}

/// The meta keys [`decode_meta`] reads, in its order.
pub(crate) const META_KEYS: [&[u8]; 7] = [
    meta_key::ANCHOR,
    meta_key::ANCHOR_STATE,
    meta_key::NEXT_ID,
    meta_key::TIP,
    meta_key::BEST_HEADER,
    meta_key::SYNC_SEQ,
    meta_key::CL_ENABLED_OBSERVED,
];

/// The [`dash_chain::ChainMeta`] in the `meta` rows of [`META_KEYS`] and the two `locks` rows;
/// `None` when the store has none (no header step ever committed).
pub(crate) fn decode_meta(
    rows: &[Option<Vec<u8>>],
    best: Option<&[u8]>,
    floor: Option<&[u8]>,
) -> Result<Option<dash_chain::ChainMeta>, BadRow> {
    if rows.iter().all(Option::is_none) {
        return Ok(None);
    }
    let need = |i: usize| {
        rows[i]
            .as_deref()
            .ok_or_else(|| bad("meta", format!("no {} row", String::from_utf8_lossy(META_KEYS[i]))))
    };
    let anchor = need(0)?;
    if anchor.len() != 36 {
        return Err(bad("meta", "anchor is not hash ‖ height"));
    }
    let anchor_state = match need(1)? {
        [0] => AnchorState::Candidate,
        [1] => AnchorState::Established,
        s => return Err(bad("meta", format!("anchor_state {s:?}"))),
    };
    let enabled_observed = match need(6)? {
        [0] => false,
        [1] => true,
        s => return Err(bad("meta", format!("cl_enabled_observed {s:?}"))),
    };
    let best = best
        .map(|b| match b.len() {
            36 => Ok(dash_chain::ChainLockRef {
                height: read_u32("locks", &b[..4])?,
                hash: read_hash("locks", &b[4..])?.into(),
            }),
            n => Err(bad("locks", format!("best: {n} bytes"))),
        })
        .transpose()?;
    let floor = floor
        .map(|f| match f.len() {
            40 => Ok(BlockId(read_u32("locks", &f[..4])?)),
            n => Err(bad("locks", format!("floor: {n} bytes"))),
        })
        .transpose()?;
    Ok(Some(dash_chain::ChainMeta {
        schema: SPV_SCHEMA.version,
        anchor: (read_hash("meta", &anchor[..32])?.into(), read_u32("meta", &anchor[32..])?),
        anchor_state,
        next_id: BlockId(read_u32("meta", need(2)?)?),
        tip: BlockId(read_u32("meta", need(3)?)?),
        best_header: BlockId(read_u32("meta", need(4)?)?),
        sync_seq: read_u64("meta", need(5)?)?,
        cl: ClPersisted {
            best,
            floor,
            enabled_observed,
        },
    }))
}

/// A subscriber with a durable locator (SB0 §9.5). The wallet has none (DEC-112 O4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subscriber {
    /// `cfhdr`.
    FilterHeaders,
    /// `cfilter`.
    Filters,
    /// `blocks`: wallet-matched blocks.
    Blocks,
}

impl Subscriber {
    pub const ALL: [Subscriber; 3] =
        [Subscriber::FilterHeaders, Subscriber::Filters, Subscriber::Blocks];

    pub(crate) fn cf(self) -> Cf {
        match self {
            Self::FilterHeaders => CFHDR,
            Self::Filters => CFILTER,
            Self::Blocks => BLOCKS,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::FilterHeaders => "cfhdr",
            Self::Filters => "cfilter",
            Self::Blocks => "blocks",
        }
    }

    pub(crate) fn locator_key(self) -> &'static [u8] {
        self.name().as_bytes()
    }
}

/// A `locators` value: the hash and height of the block a subscriber's data reaches.
pub(crate) fn encode_locator(hash: &[u8; 32], height: u32) -> [u8; 36] {
    let mut v = [0; 36];
    v[..32].copy_from_slice(hash);
    v[32..].copy_from_slice(&height.to_be_bytes());
    v
}

pub(crate) fn decode_locator(v: &[u8]) -> Result<([u8; 32], u32), BadRow> {
    if v.len() != 36 {
        return Err(bad("locators", format!("{} bytes, not 36", v.len())));
    }
    Ok((read_hash("locators", &v[..32])?, read_u32("locators", &v[32..])?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_family_indexes_match_the_schema() {
        for (cf, name) in [
            (META, "meta"),
            (HDR, "hdr"),
            (HDR_HASH, "hdr_hash"),
            (HDR_H, "hdr_h"),
            (HDR_PREV, "hdr_prev"),
            (HDR_LEAF, "hdr_leaf"),
            (ACTIVE, "active"),
            (LOCKS, "locks"),
            (CFHDR, "cfhdr"),
            (CFILTER, "cfilter"),
            (BLOCKS, "blocks"),
            (LOCATORS, "locators"),
            (MN_LOG, "mn_log"),
            (HDR_RUNS, "hdr_runs"),
        ] {
            assert_eq!(SPV_SCHEMA.cf(name), Some(cf), "{name}");
        }
        SPV_SCHEMA.validate();
        assert_eq!(SPV_SCHEMA.version, 3);
    }

    #[test]
    fn hdr_row_layout_round_trips() {
        let row = HeaderRow {
            hash: [7; 32],
            id: 9,
            height: 0x0102_0304,
            header: [3; 80],
            skip: Some(5),
            status: 2 | 128,
            chain_work: [0xab; 32],
            time_max: 77,
            kind: 1,
        };
        let v = encode_row(&row);
        assert_eq!(v.len(), ROW_LEN);
        assert_eq!(&v[32..36], &[1, 2, 3, 4], "height big-endian after the hash");
        assert_eq!(decode_row(9, &v), Ok(row));
        let none = HeaderRow {
            skip: None,
            ..row
        };
        assert_eq!(decode_row(9, &encode_row(&none)), Ok(none));
        assert!(decode_row(9, &v[1..]).is_err());
        let mut flag = v.clone();
        flag[116] = 2;
        assert!(decode_row(9, &flag).is_err());
    }

    #[test]
    fn keys_order_numerically() {
        assert!(height_id_key(255, BlockId(9)) < height_id_key(256, BlockId(0)));
        assert!(prev_id_key(&[1; 32], BlockId(300)) > prev_id_key(&[1; 32], BlockId(2)));
        assert_eq!(trailing_id("hdr_h", &height_id_key(4, BlockId(77))), Ok(BlockId(77)));
    }

    #[test]
    fn hdr_hash_values_round_trip() {
        let v = hash_value(BlockId(0x0102_0304), 0x0506_0708);
        assert_eq!(v, [1, 2, 3, 4, 5, 6, 7, 8], "id then height, big-endian");
        assert_eq!(read_hash_value(&v), Ok((BlockId(0x0102_0304), 0x0506_0708)));
        assert!(read_hash_value(&v[..4]).is_err(), "a bare id is not a value");
    }
}
