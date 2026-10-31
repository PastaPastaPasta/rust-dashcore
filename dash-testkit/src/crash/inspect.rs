//! C0, the offline inspector: what a crashed image's chainstate holds before rdashd touches it.
//!
//! It opens a datadir copy of its own (the open replays RocksDB's WAL, which writes), as recovery's
//! steps 0–3 open it (`ChainstateOpen::open`), and checks:
//! - the decision keys against recovery's preconditions (CH2 §4.3 step 5): with a marker
//!   `[new, old]`, `new` is in the index with its data, `old` is null or in the index, and
//!   `evo_tip == new`; without one, `tip` is present and equals `evo_tip`, or the chainstate is
//!   empty (DEC-202);
//! - I1, data before reference: every `block_index` record with HAVE_DATA points at blk bytes that
//!   begin with the fixture's block of its key (without a fixture: whose header hashes to the key
//!   and whose transactions decode to the header's merkle root), every record with HAVE_UNDO at a
//!   rev record whose checksum holds, and every `block_files` size is at most its file's length.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use dash_chain::{BlockIndexRecord, Status};
use dash_consensus::primitives::BlockHash;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_chainstate::caches::{CacheSizes, IndexArgs};
use dashd_chainstate::chain::RecordingAbort;
use dashd_chainstate::flush::NullObserver;
use dashd_chainstate::recovery::ChainstateOpen;
use dashd_chainstate::store::{GlobalFailStop, GuardedEngine, Meta};
use dashd_chainstate::tuning::{EngineTuning, RocksTuning};
use dashd_store::flatfile::{BlockFilesConfig, FlatFilePos, OsFs};

use super::fixture::{Fixture, hex};

/// The decision keys as C0 reads them, for the phase report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Keys {
    /// No tip, no marker, no evo tip: nothing was ever flushed.
    Empty,
    /// A durable tip (hex, display order), no marker.
    Tip(String),
    /// A marker: the flush to `new` from `old` (null for a first flush) began and did not end.
    Marker {
        new: String,
        old: Option<String>,
    },
}

#[derive(Debug)]
pub struct Inspection {
    pub keys: Keys,
    pub records: usize,
    /// The blocks the index says it has the data of (hex, display order, sorted): C4 samples them.
    pub with_data: Vec<String>,
    pub failures: Vec<String>,
}

fn display(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// Recovery's preconditions on the decision keys (CH2 §4.3 step 5), with `index` mapping each
/// `block_index` key to whether its record has the block's data.
fn decision_keys(meta: &Meta, index: &HashMap<[u8; 32], bool>, failures: &mut Vec<String>) -> Keys {
    let bytes = |h: &BlockHash| *h.as_bytes();
    let shown = |h: Option<&BlockHash>| h.map_or("null".to_owned(), |h| display(&bytes(h)));
    let have = |h: &[u8; 32]| index.get(h).copied().unwrap_or(false);
    match (meta.head_blocks, meta.tip) {
        (Some((new, old)), _) => {
            let new_b = bytes(&new);
            if !have(&new_b) {
                failures.push(format!("marker: new {} has no stored data", display(&new_b)));
            }
            if let Some(o) = old.as_ref().map(bytes).filter(|o| !index.contains_key(o)) {
                failures.push(format!("marker: old {} is not in the index", display(&o)));
            }
            if meta.evo_tip != Some(new) {
                failures.push(format!(
                    "marker: evo_tip {} is not new {}",
                    shown(meta.evo_tip.as_ref()),
                    display(&new_b)
                ));
            }
            Keys::Marker {
                new: display(&new_b),
                old: old.as_ref().map(|o| display(&bytes(o))),
            }
        }
        (None, Some(tip)) => {
            let tip_b = bytes(&tip);
            if meta.evo_tip != Some(tip) {
                failures.push(format!(
                    "tip {} but evo_tip {}",
                    display(&tip_b),
                    shown(meta.evo_tip.as_ref())
                ));
            }
            if !have(&tip_b) {
                failures.push(format!("tip {} has no stored data", display(&tip_b)));
            }
            Keys::Tip(display(&tip_b))
        }
        (None, None) => {
            if let Some(evo) = &meta.evo_tip {
                failures
                    .push(format!("no tip and no marker, but evo_tip {}", display(&bytes(evo))));
            }
            Keys::Empty
        }
    }
}

/// I1 on one HAVE_DATA record: `buf`, the blk record's bytes, against the block of `key`. With a
/// fixture the bytes must begin with its block (a record may hold bytes after the block, see
/// `read_block`); without one the header must hash to `key` and the block must decode to its
/// header's merkle root. A header kept over a lost body fails either way.
fn check_block(key: &[u8; 32], buf: &[u8], fixture: Option<&Fixture>) -> Option<String> {
    let shown = display(key);
    if buf.len() < 80 {
        return Some(format!("{shown}: short block"));
    }
    let hash = dash_consensus::hash::x11(&buf[..80]);
    if &hash != key {
        return Some(format!("{shown} holds block {}", display(&hash)));
    }
    if let Some(fixture) = fixture {
        let Some(want) = fixture.block_hex(&shown) else {
            return Some(format!("{shown} is no block of the fixture"));
        };
        let n = want.len() / 2;
        return (buf.len() < n || hex(&buf[..n]) != want)
            .then(|| format!("{shown}: stored bytes differ from the fixture's block"));
    }
    match dash_consensus::decode_block(&mut &buf[..]) {
        Ok(block) => {
            let block = block.value();
            (block.merkle().0 != block.header().merkle_root())
                .then(|| format!("{shown}: transactions do not hash to the header's merkle root"))
        }
        Err(e) => Some(format!("{shown}: does not decode: {e:?}")),
    }
}

/// The chainstate of the regtest datadir `datadir` (`<datadir>/regtest/{chainstate,blocks}`),
/// with I1 against `fixture`'s blocks if one is given.
pub fn inspect(datadir: &Path, fixture: Option<&Fixture>) -> Result<Inspection, String> {
    let net = datadir.join("regtest");
    let sizes = CacheSizes::calculate(4, &IndexArgs::default());
    let (engine, _) = RocksTuning::default()
        .open_chainstate(&net.join("chainstate"), &sizes, 1 << 20)
        .map_err(|e| format!("opening the chainstate: {e}"))?;
    let engine =
        GuardedEngine::new(engine, GlobalFailStop::default(), Arc::new(RecordingAbort::default()));
    let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
        .build()
        .map_err(|e| format!("regtest's parameters: {e:?}"))?;
    let obs = NullObserver;
    let opened = ChainstateOpen::open(
        &engine,
        Arc::new(OsFs::read_only()),
        BlockFilesConfig::new(net.join("blocks"), params.message_start),
        &obs,
    )
    .map_err(|e| format!("recovery's open: {e:?}"))?;
    let meta = *opened.meta();
    let mut failures = Vec::new();
    let mut index: HashMap<[u8; 32], BlockIndexRecord> = HashMap::new();
    for item in opened.block_index() {
        let (key, value) = item.map_err(|e| format!("reading block_index: {e}"))?;
        let Ok(key) = <[u8; 32]>::try_from(key.as_slice()) else {
            failures.push(format!("a block_index key of {} bytes", key.len()));
            continue;
        };
        match BlockIndexRecord::decode(&value) {
            Ok(r) => {
                index.insert(key, r);
            }
            Err(e) => failures.push(format!("block_index {}: {e:?}", display(&key))),
        }
    }
    let present: HashMap<[u8; 32], bool> =
        index.iter().map(|(k, r)| (*k, r.status.has(Status::HAVE_DATA))).collect();
    let keys = decision_keys(&meta, &present, &mut failures);
    // I1.
    let files = opened.files();
    let reader = files.read(|f| f.reader());
    let mut with_data = Vec::new();
    let mut buf = Vec::new();
    for (key, r) in &index {
        let file = r.file.unwrap_or(0).max(0) as u32;
        if r.status.has(Status::HAVE_DATA) {
            with_data.push(display(key));
            let pos = FlatFilePos::new(file, r.data_pos.unwrap_or(0));
            buf.clear();
            match reader.read_block(pos, &mut buf) {
                Ok(()) => {
                    if let Some(f) = check_block(key, &buf, fixture) {
                        failures.push(format!("I1: {f} (at {pos:?})"));
                    }
                }
                Err(e) => failures.push(format!("I1: {} at {pos:?}: {e}", display(key))),
            }
        }
        if r.status.has(Status::HAVE_UNDO) {
            let pos = FlatFilePos::new(file, r.undo_pos.unwrap_or(0));
            // The checksum covers the parent's hash.
            let prev = *r.header.prev_hash.as_bytes();
            buf.clear();
            if let Err(e) = reader.read_undo(pos, &prev, &mut buf) {
                failures.push(format!("I1: undo of {} at {pos:?}: {e}", display(key)));
            }
        }
    }
    files.read(|f| {
        for (n, info) in f.infos().iter().enumerate() {
            for (kind, size) in [("blk", info.size), ("rev", info.undo_size)] {
                let path = net.join("blocks").join(format!("{kind}{n:05}.dat"));
                let len = std::fs::metadata(&path).map_or(0, |m| m.len());
                if u64::from(size) > len {
                    failures
                        .push(format!("I1: block_files {n} says {kind} {size} B, file {len} B"));
                }
            }
        }
    });
    with_data.sort();
    Ok(Inspection {
        keys,
        records: index.len(),
        with_data,
        failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> BlockHash {
        BlockHash::from([b; 32])
    }

    fn check(meta: Meta, index: &[(u8, bool)]) -> (Keys, Vec<String>) {
        let index = index.iter().map(|&(b, data)| ([b; 32], data)).collect();
        let mut failures = Vec::new();
        (decision_keys(&meta, &index, &mut failures), failures)
    }

    #[test]
    fn a_marker_needs_new_with_data_old_indexed_and_evo_at_new() {
        let marker = |old| Meta {
            head_blocks: Some((h(2), old)),
            evo_tip: Some(h(2)),
            ..Meta::default()
        };
        let (keys, failures) = check(marker(Some(h(1))), &[(1, true), (2, true)]);
        assert!(failures.is_empty(), "{failures:?}");
        assert!(matches!(
            keys,
            Keys::Marker {
                old: Some(_),
                ..
            }
        ));
        // A first flush's marker has a null old.
        assert!(check(marker(None), &[(2, true)]).1.is_empty());
        // new without its data, old not in the index, evo_tip elsewhere: one failure each.
        assert_eq!(check(marker(Some(h(1))), &[(1, true), (2, false)]).1.len(), 1);
        assert_eq!(check(marker(Some(h(1))), &[(2, true)]).1.len(), 1);
        let evo_elsewhere = Meta {
            evo_tip: Some(h(1)),
            ..marker(Some(h(1)))
        };
        assert_eq!(check(evo_elsewhere, &[(1, true), (2, true)]).1.len(), 1);
    }

    #[test]
    fn without_a_marker_the_tip_is_the_evo_tip_or_the_store_is_empty() {
        let tip = Meta {
            tip: Some(h(3)),
            evo_tip: Some(h(3)),
            ..Meta::default()
        };
        let (keys, failures) = check(tip, &[(3, true)]);
        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(keys, Keys::Tip(display(&[3; 32])));
        let evo_behind = Meta {
            evo_tip: Some(h(2)),
            ..tip
        };
        assert_eq!(check(evo_behind, &[(2, true), (3, true)]).1.len(), 1);
        assert_eq!(check(tip, &[(3, false)]).1.len(), 1);
        assert_eq!(check(Meta::default(), &[]), (Keys::Empty, Vec::new()));
        let evo_only = Meta {
            evo_tip: Some(h(1)),
            ..Meta::default()
        };
        assert_eq!(check(evo_only, &[]).1.len(), 1);
    }

    /// M-1 (ST3b r1): a stored header whose body was lost (zeroed) fails I1, with or without a
    /// fixture; the intact block, and one with bytes after it, pass.
    #[test]
    fn a_kept_header_over_a_zeroed_body_fails_i1() {
        let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .build()
            .expect("regtest params");
        let block = dashd_chainstate::chain::genesis_block(&params);
        let bytes = dash_consensus::core_serialize(&*block);
        let key = *block.header().hash().as_bytes();
        let mut fixture = Fixture::default();
        fixture.blocks.insert(display(&key), hex(&bytes));
        let mut trailing = bytes.clone();
        trailing.extend_from_slice(&[0; 7]);
        let mut zeroed = bytes.clone();
        zeroed[80..].fill(0);
        for f in [None, Some(&fixture)] {
            assert_eq!(check_block(&key, &bytes, f), None, "{}", f.is_some());
            assert_eq!(check_block(&key, &trailing, f), None, "{}", f.is_some());
            assert!(check_block(&key, &zeroed, f).is_some(), "{}", f.is_some());
            assert!(check_block(&key, &bytes[..79], f).is_some(), "{}", f.is_some());
        }
        // With a fixture, a block it does not hold fails.
        assert!(check_block(&key, &bytes, Some(&Fixture::default())).is_some());
    }
}
