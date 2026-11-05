//! [`ChainRefs`]: CH2's [`RefExtractor`] (CH2 §6.4), ST3a §6.2's over CH2's records with the
//! same `finalizes` scopes.

use dash_chain::{BlockIndexRecord, Status};
use dash_consensus::primitives::BlockHash;
use dashd_store::faulty::{
    FileRef, Hash, Peek, RefExtractor, Requirement, Scope, WorldDurableView,
};
use dashd_store::{Cf, DbSchema};

use super::stores::cf as chain_cf;
use super::subs::SUB_DISABLED;
use super::synth::{RECORD_HEADER, lookup};
use super::{BLOCKS_DIR, CHAIN_STORE, SUB_STORE, common_prefix};
use crate::store::{HEAD_BLOCKS, TIP};
use crate::subscriber::{TOY_BEST, TOY_META, decode_locator};

fn index_cf() -> Cf {
    chain_cf("block_index")
}

/// A file of `blocks/`: `blk` or `rev`, by number.
fn blocks_path(prefix: &str, file: i32) -> String {
    format!("{BLOCKS_DIR}/{prefix}{file:05}.dat")
}

/// The durable index path from genesis to `tip`, or `None` while an entry on it is undecided.
fn durable_path(view: &WorldDurableView, tip: Hash) -> Option<Vec<Hash>> {
    let mut p = Vec::new();
    let mut cur = tip;
    loop {
        p.push(cur);
        match view.get(CHAIN_STORE, index_cf(), &cur) {
            Peek::Known(Some(v)) => {
                let r = BlockIndexRecord::decode(&v).ok()?;
                if r.height == 0 {
                    break;
                }
                cur = *r.header.prev_hash.as_bytes();
            }
            Peek::Known(None) => break,
            Peek::Unknown => return None,
        }
    }
    p.reverse();
    Some(p)
}

/// The tip recovery reaches from the durable chainstate: the marker's `new` (the replay rolls
/// forward to it), else `tip`.
fn durable_tip(view: &WorldDurableView) -> Peek<Option<Hash>> {
    let hash = |v: &[u8]| <[u8; 32]>::try_from(v).ok();
    match view.get(CHAIN_STORE, DbSchema::META, HEAD_BLOCKS) {
        Peek::Unknown => Peek::Unknown,
        Peek::Known(Some(m)) => Peek::Known(m.get(..32).and_then(hash)),
        Peek::Known(None) => match view.get(CHAIN_STORE, DbSchema::META, TIP) {
            Peek::Known(v) => Peek::Known(v.and_then(|v| hash(&v))),
            Peek::Unknown => Peek::Unknown,
        },
    }
}

/// The subscriber's durable locator.
fn locator(view: &WorldDurableView) -> Option<Hash> {
    match view.get(SUB_STORE, TOY_META, TOY_BEST) {
        Peek::Known(Some(v)) => decode_locator(&v).map(|h| *h.as_bytes()),
        _ => None,
    }
}

/// A block requirement on each of `hashes`, with undo or on its data only.
fn block_reqs(hashes: &[Hash], undo: bool) -> impl Iterator<Item = Requirement> + '_ {
    hashes.iter().map(move |&hash| Requirement::Block {
        hash,
        undo,
    })
}

/// CH2's dependency declarations (ST3a §5.3):
/// - a `block_index` entry with `HAVE_DATA` (`HAVE_UNDO`) requires its block's (undo record's)
///   bytes, header and checksum included, as CH1-node's `LoadBlockIndex` and the replay read
///   them;
/// - the marker `new ‖ old` requires its replay path: undo on the disconnects from `old` to the
///   fork, data on the connects to `new`;
/// - the subscriber's locator L (§2.9 item 7, ST3a's toy subscriber) requires the chainstate's
///   durable tip G not behind it (L not a strict descendant of G, R11a), undo on L's path down to
///   the fork with G, and data on G's path up from it (R11b);
/// - `tip = new` (Bk, a one-batch B1, recovery's final batch) finalizes its flush's (or
///   recovery's) coin sub-batches, a pending presumed effect and the previous finalizer; the
///   marker finalizes the presumed effect and the previous finalizer. A range delete (the
///   coins' tombstone, the wipe) names no record and finalizes nothing.
pub struct ChainRefs;

impl RefExtractor for ChainRefs {
    fn refs(
        &self,
        db: &str,
        cf: Cf,
        key: &[u8],
        value: &[u8],
        view: &WorldDurableView,
    ) -> Vec<Requirement> {
        if db == SUB_STORE {
            return subscriber_refs(cf, key, value, view);
        }
        if db != CHAIN_STORE {
            return Vec::new();
        }
        let mut out = Vec::new();
        if cf == DbSchema::META && key == HEAD_BLOCKS && value.len() == 64 {
            let new: Hash = value[..32].try_into().expect("32 bytes");
            let old: Hash = value[32..].try_into().expect("32 bytes");
            let np = durable_path(view, new);
            // With `old` null the replay starts at height 1: genesis is never replayed.
            let op = if old == [0; 32] {
                np.as_ref().map(|p| p[..1].to_vec())
            } else {
                durable_path(view, old)
            };
            if let (Some(np), Some(op)) = (np, op) {
                let fork = common_prefix(&np, &op);
                out.extend(block_reqs(&op[fork..], true));
                out.extend(block_reqs(&np[fork..], false));
            }
        }
        if cf == index_cf()
            && let Ok(r) = BlockIndexRecord::decode(value)
            && let Ok(hash) = <[u8; 32]>::try_from(key)
            && let Some(g) = lookup(&BlockHash::from(hash))
        {
            let file = r.file.unwrap_or(0);
            let has =
                |pos: Option<u32>, flag| pos.filter(|p| *p >= RECORD_HEADER && r.status.has(flag));
            if let Some(pos) = has(r.data_pos, Status::HAVE_DATA) {
                let at = u64::from(pos - RECORD_HEADER);
                out.push(Requirement::Bytes(FileRef::new(
                    blocks_path("blk", file),
                    at,
                    g.data_len(),
                )));
            }
            if let Some(pos) = has(r.undo_pos, Status::HAVE_UNDO) {
                let at = u64::from(pos - RECORD_HEADER);
                out.push(Requirement::Bytes(FileRef::new(
                    blocks_path("rev", file),
                    at,
                    g.undo_len(),
                )));
            }
        }
        out
    }

    /// A block requirement needs a durable index entry with a data position (with undo, an undo
    /// position too); its bytes are the entry's own requirement.
    fn resolve(&self, req: &Requirement, view: &WorldDurableView) -> Result<Vec<FileRef>, String> {
        match req {
            Requirement::Bytes(r) => Ok(vec![r.clone()]),
            Requirement::DurableTipAtLeast {
                hash,
                ..
            } => {
                // Only while the locator still names L.
                if locator(view) != Some(*hash) {
                    return Ok(Vec::new());
                }
                match durable_tip(view) {
                    Peek::Unknown => Ok(Vec::new()),
                    Peek::Known(None) => {
                        Err("the subscriber's locator is ahead of an empty chainstate".into())
                    }
                    Peek::Known(Some(g)) => match durable_path(view, *hash) {
                        Some(l) if l.len() > 1 && l[..l.len() - 1].contains(&g) => Err(format!(
                            "the subscriber's locator at {} is ahead of the chainstate's durable tip",
                            l.len() - 1
                        )),
                        _ => Ok(Vec::new()),
                    },
                }
            }
            Requirement::Block {
                hash,
                undo,
            } => match view.get(CHAIN_STORE, index_cf(), hash) {
                Peek::Unknown => Ok(Vec::new()),
                Peek::Known(None) => Err("a replay-path block has no durable index entry".into()),
                Peek::Known(Some(v)) => {
                    let r = BlockIndexRecord::decode(&v).map_err(|_| "a corrupt index entry")?;
                    let (need, what) = if *undo {
                        (Status::HAVE_DATA | Status::HAVE_UNDO, "undo")
                    } else {
                        (Status::HAVE_DATA, "data")
                    };
                    if r.status.0 & need != need {
                        return Err(format!(
                            "replay-path block {} has no durable {what} position",
                            r.height
                        ));
                    }
                    Ok(Vec::new())
                }
            },
        }
    }

    fn finalizes(&self, db: &str, cf: Cf, key: &[u8], value: Option<&[u8]>) -> Option<Vec<Scope>> {
        // A locator commit presumes the subscriber's batches since its previous one.
        if db == SUB_STORE && cf == TOY_META && key == TOY_BEST && value.is_some() {
            return Some(vec![Scope::SincePreviousLocator {
                store: SUB_STORE,
            }]);
        }
        if db != CHAIN_STORE || cf != DbSchema::META || value.is_none() {
            return None;
        }
        let effect = Scope::Keys {
            cf: chain_cf("llmq_snapshot"),
            range: b"snap".to_vec()..b"snaq".to_vec(),
        };
        match key {
            k if k == TIP => Some(vec![Scope::SinceFlushStart, effect, Scope::PreviousFinalizer]),
            k if k == HEAD_BLOCKS => Some(vec![effect, Scope::PreviousFinalizer]),
            _ => None,
        }
    }
}

/// The subscriber's locator L: G at least L, undo down from L to the fork with G's durable path,
/// data up from it to G. While a record is undecided the requirements stand (ST3a §5.3).
fn subscriber_refs(cf: Cf, key: &[u8], value: &[u8], view: &WorldDurableView) -> Vec<Requirement> {
    if cf != TOY_META || key != TOY_BEST {
        return Vec::new();
    }
    // A disabled index holds no lock: its locator requires nothing (S11c).
    if let Peek::Known(Some(_)) = view.get(SUB_STORE, TOY_META, SUB_DISABLED) {
        return Vec::new();
    }
    let Some(l) = decode_locator(value).map(|h| *h.as_bytes()) else {
        return Vec::new();
    };
    let mut out = vec![Requirement::DurableTipAtLeast {
        store: CHAIN_STORE,
        hash: l,
    }];
    let gp = match durable_tip(view) {
        Peek::Known(Some(g)) => durable_path(view, g),
        Peek::Known(None) => Some(Vec::new()),
        Peek::Unknown => None,
    };
    if let (Some(lp), Some(gp)) = (durable_path(view, l), gp) {
        let fork = common_prefix(&lp, &gp);
        out.extend(block_reqs(&lp[fork..], true));
        out.extend(block_reqs(&gp[fork..], false));
    }
    out
}
