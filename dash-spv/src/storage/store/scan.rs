//! Ordered reads that `KvRead` has no primitive for: from a key up, and the last key.
//!
//! `KvRead` scans by prefix only. A key range `[start, ∞)` over fixed-length big-endian keys is
//! a union of prefixes: `start` without its last byte (filtered to keys at or above `start`),
//! then each larger value of every earlier byte. An empty prefix costs one query, so a range
//! over 4-byte keys costs at most 3 × 255 + 1 of them, a few milliseconds on SQLite.
//!
//! Every scan here is collected before the next read starts: a live iterator holds one of the
//! engine's pooled read connections (`KvEngine::snapshot`'s rule), and the next prefix's scan
//! must not wait for it.

use dash_kv::{Cf, KvRead, Result};

/// Calls `f` on each entry of `cf` whose key is at or above `start`, in key order, until `f`
/// returns false. `start` is a full key of the column family's fixed key length.
pub(crate) fn for_each_from(
    db: &impl KvRead,
    cf: Cf,
    start: &[u8],
    mut f: impl FnMut(&[u8], &[u8]) -> bool,
) -> Result<()> {
    let Some((_, head)) = start.split_last() else {
        return for_each_prefix(db, cf, &[], |k, v| f(k, v)).map(|_| ());
    };
    // `start` and the keys after it that share every byte but the last.
    let mut go = for_each_prefix(db, cf, head, |k, v| k < start || f(k, v))?;
    // Then, from the second-to-last byte up to the first, each larger value of that byte: the
    // keys that agree with `start` before it and exceed it there, in key order.
    for i in (0..head.len()).rev() {
        let mut prefix = start[..=i].to_vec();
        for b in u16::from(start[i]) + 1..=u16::from(u8::MAX) {
            if !go {
                return Ok(());
            }
            prefix[i] = b as u8;
            go = for_each_prefix(db, cf, &prefix, &mut f)?;
        }
    }
    Ok(())
}

/// Calls `f` on each entry with `prefix`; false once `f` asked to stop. The scan is read in
/// full before `f` runs, so `f` may read the engine.
fn for_each_prefix(
    db: &impl KvRead,
    cf: Cf,
    prefix: &[u8],
    mut f: impl FnMut(&[u8], &[u8]) -> bool,
) -> Result<bool> {
    let rows = db.iter_prefix(cf, prefix).collect::<Result<Vec<_>>>()?;
    for (k, v) in &rows {
        if !f(k, v) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The entries of `cf` from `start` up, at most `limit` of them.
pub(crate) fn collect_from(
    db: &impl KvRead,
    cf: Cf,
    start: &[u8],
    limit: usize,
) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut out = Vec::new();
    if limit == 0 {
        return Ok(out);
    }
    for_each_from(db, cf, start, |k, v| {
        out.push((k.to_vec(), v.to_vec()));
        out.len() < limit
    })?;
    Ok(out)
}

/// Whether any key of `cf` starts with `prefix`.
fn any_with(db: &impl KvRead, cf: Cf, prefix: &[u8]) -> Result<bool> {
    db.iter_prefix(cf, prefix).next().transpose().map(|e| e.is_some())
}

/// The largest key of `cf`, whose keys are all `len` bytes long: found byte by byte, each from
/// the top down (at most 256 probes per byte).
pub(crate) fn last_key(db: &impl KvRead, cf: Cf, len: usize) -> Result<Option<Vec<u8>>> {
    let mut key = Vec::with_capacity(len);
    while key.len() < len {
        let mut found = false;
        for b in (0..=u8::MAX).rev() {
            key.push(b);
            if any_with(db, cf, &key)? {
                found = true;
                break;
            }
            key.pop();
        }
        if !found {
            return Ok(None);
        }
    }
    Ok(Some(key))
}

/// The smallest key of `cf`.
pub(crate) fn first_key(db: &impl KvRead, cf: Cf) -> Result<Option<Vec<u8>>> {
    db.iter_prefix(cf, &[]).next().transpose().map(|e| e.map(|(k, _)| k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_kv::{CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, MemoryEngine};

    static SCHEMA: DbSchema = DbSchema {
        name: "scan",
        version: 1,
        cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("t", CfProfile::Ordered)],
    };
    const T: Cf = Cf::new(1);

    fn db(keys: &[u32]) -> MemoryEngine {
        let db = MemoryEngine::new(&SCHEMA);
        let mut b = db.batch();
        for k in keys {
            b.put(T, &k.to_be_bytes(), &[1]);
        }
        db.write(b, Commit::Buffered).unwrap();
        db
    }

    fn from(db: &MemoryEngine, start: u32) -> Vec<u32> {
        collect_from(db, T, &start.to_be_bytes(), usize::MAX)
            .unwrap()
            .into_iter()
            .map(|(k, _)| u32::from_be_bytes(k.try_into().unwrap()))
            .collect()
    }

    #[test]
    fn ranges_match_a_filter_over_every_key() {
        let keys = [0, 1, 255, 256, 257, 65_535, 65_536, 0x00ff_ffff, 0x0100_0000, 2_400_000];
        let mut keys: Vec<u32> = keys.into_iter().chain([u32::MAX - 1, u32::MAX]).collect();
        keys.sort_unstable();
        let db = db(&keys);
        for start in [0, 1, 2, 255, 256, 258, 65_535, 65_537, 2_399_999, 2_400_001, u32::MAX] {
            let want: Vec<u32> = keys.iter().copied().filter(|&k| k >= start).collect();
            assert_eq!(from(&db, start), want, "from {start}");
        }
        assert_eq!(collect_from(&db, T, &256u32.to_be_bytes(), 2).unwrap().len(), 2);
    }

    #[test]
    fn last_and_first_keys() {
        assert_eq!(last_key(&db(&[]), T, 4).unwrap(), None);
        for keys in [&[7u32][..], &[0, 9, 2_400_000], &[0, u32::MAX], &[0x0100_0000, 3]] {
            let d = db(keys);
            let max = keys.iter().max().unwrap().to_be_bytes().to_vec();
            assert_eq!(last_key(&d, T, 4).unwrap(), Some(max), "{keys:?}");
            let min = keys.iter().min().unwrap().to_be_bytes().to_vec();
            assert_eq!(first_key(&d, T).unwrap(), Some(min));
        }
    }
}
