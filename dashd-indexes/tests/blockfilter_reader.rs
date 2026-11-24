// SPDX-License-Identifier: MIT

//! The basic block filter index's reader over the rows and `fltr?????.dat` files the real data
//! object wrote, against the recorded `getblockfilter` answers of dashd v24.0.0.

mod common;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::blockfilter::*;
use dash_chain::{BlockRef, IndexAccess};
use dash_consensus::{BlockId, decode_tx};
use dash_filters::{BlockFilter, FilterHeader, extend_headers};
use dash_primitives::{BlockHash, Transaction};
use dash_types::Numeric as _;
use dashd_chainstate::subscriber::{FileAppend, FilePos, height_key};
use dashd_indexes::blockfilter::codec::{DbVal, FilterPos, HeightValue};
use dashd_indexes::blockfilter::data::{BLOCKFILTER_DB, HEIGHT};
use dashd_indexes::blockfilter::files::encode_entry;
use dashd_indexes::blockfilter::{FilterFiles, StoredFilters};
use dashd_indexes::{Ancestry, FilterIndexReader, IndexError, IndexStatus, IndexSummary};
use dashd_store::{Cf, Commit, ErrorKind, KvBatch, KvEngine, KvRead, MemoryEngine, StoreError};
use serde_json::Value;

struct Synced;

impl IndexStatus for Synced {
    fn summary(&self) -> IndexSummary {
        IndexSummary {
            name: "basic block filter index",
            synced: true,
            best_block_height: 0,
        }
    }

    fn block_until_synced(&self) -> bool {
        true
    }
}

/// The run's store, with reads that fail once armed.
struct Faulty {
    db: Arc<Db>,
    fail: AtomicBool,
}

impl Faulty {
    fn check(&self) -> dashd_store::Result<()> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(StoreError::engine("test", ErrorKind::Io, "an injected read failure"));
        }
        Ok(())
    }
}

impl KvRead for Faulty {
    fn get(&self, cf: Cf, key: &[u8]) -> dashd_store::Result<Option<Vec<u8>>> {
        self.check()?;
        self.db.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> dashd_store::Result<Vec<Option<Vec<u8>>>> {
        self.check()?;
        self.db.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = dashd_store::Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.db.iter_prefix(cf, prefix)
    }
}

fn reader(run: &Run) -> StoredFilters<Faulty> {
    let db = Faulty {
        db: Arc::clone(&run.db),
        fail: AtomicBool::new(false),
    };
    StoredFilters::new(Arc::new(db), run.files(), Arc::new(Synced))
}

struct Branches<'a>(&'a RecordedIndex);

impl Ancestry for Branches<'_> {
    fn ancestor(&self, block: &BlockRef, height: u32) -> Option<BlockRef> {
        self.0.ancestor(block.id, height).map(|id| self.0.block(id))
    }
}

/// The block's filter as dashd recorded it, keyed by its hash.
fn recorded(run: &Run, b: &BlockRef) -> (BlockFilter, String) {
    let (encoded, header) = &run.rec.filters[&b.hash];
    (BlockFilter::from_encoded(&b.hash, encoded.clone(), false).unwrap(), header.clone())
}

/// The recorded blocks that are no other recorded block's parent.
fn tips(index: &RecordedIndex) -> Vec<BlockRef> {
    let parents: HashSet<BlockId> = index.nodes.iter().filter_map(|n| n.1).collect();
    (0..index.nodes.len() as u32)
        .map(BlockId)
        .filter(|id| !parents.contains(id))
        .map(|id| index.block(id))
        .collect()
}

/// Every recorded block reads back as dashd's `getblockfilter`, reorged-out blocks through the
/// archive.
fn check_every_block(run: &Run) {
    let r = reader(run);
    let mut archived = 0;
    for hash in run.rec.filters.keys() {
        let b = run.rec.index.by_hash(hash);
        let (filter, header) = recorded(run, &b);
        assert_eq!(r.filter(&b), Ok(Some(filter)), "filter at {}", b.height);
        let got = r.filter_header(&b).unwrap().expect("a header");
        assert_eq!(got.to_string(), header, "header at {}", b.height);
        archived += usize::from(run.value(&b).unwrap().1);
    }
    assert!(archived > 0, "no block was served from the archive");
}

#[test]
fn every_g_block_reads_back_as_dashd_including_stale_ones() {
    check_every_block(&replay(None, None));
}

#[test]
fn every_g_block_reads_back_after_restarts_and_over_small_files() {
    check_every_block(&replay(None, Some(7)));
    let run = replay(Some((600, 256)), None);
    assert!(run.files().end().unwrap().file > 3);
    check_every_block(&run);
}

/// From every start along every recorded branch, active or stale: the ranges equal the
/// per-block reads in height order.
#[test]
fn ranges_along_every_branch_equal_the_per_block_reads() {
    let run = replay(Some((600, 256)), None);
    let r = reader(&run);
    let index = &run.rec.index;
    let ancestry = Branches(index);
    let tips = tips(index);
    assert!(tips.len() > 1, "g has a stale branch");
    for stop in &tips {
        let path = index.path(*stop);
        let filters: Vec<BlockFilter> = path.iter().map(|b| recorded(&run, b).0).collect();
        for start in 0..=stop.height {
            let want = &filters[start as usize..];
            let got = r.filter_range(start, stop, &ancestry).unwrap().expect("a range");
            assert_eq!(got, want, "{start}..={}", stop.height);
            let hashes: Vec<_> = want.iter().map(BlockFilter::hash).collect();
            assert_eq!(r.filter_hash_range(start, stop, &ancestry), Ok(Some(hashes)));
        }
        assert_eq!(r.filter_range(stop.height + 1, stop, &ancestry), Ok(None));
        assert_eq!(r.filter_hash_range(stop.height + 1, stop, &ancestry), Ok(None));
    }
}

/// A block the index never saw, and a stop block on top of one, are misses.
#[test]
fn missing_entries_are_misses() {
    let run = replay(None, None);
    let r = reader(&run);
    let index = &run.rec.index;
    let tip = *tips(index).iter().max_by_key(|b| b.height).unwrap();
    let unknown = BlockRef {
        id: BlockId(u32::MAX),
        hash: BlockHash::from([0xee; 32]),
        height: tip.height + 1,
    };
    assert_eq!(r.filter(&unknown), Ok(None));
    assert_eq!(r.filter_header(&unknown), Ok(None));
    let at_tip = BlockRef {
        height: tip.height,
        ..unknown
    };
    assert_eq!(r.filter(&at_tip), Ok(None));

    struct OnTip(BlockRef, RecordedIndex);
    impl Ancestry for OnTip {
        fn ancestor(&self, _: &BlockRef, height: u32) -> Option<BlockRef> {
            Branches(&self.1).ancestor(&self.0, height)
        }
    }
    let above = OnTip(tip, index.clone());
    assert_eq!(r.filter_range(0, &unknown, &above), Ok(None));
    assert_eq!(r.filter_hash_range(tip.height, &unknown, &above), Ok(None));
    assert!(r.filter_hash_range(0, &tip, &above).unwrap().is_some());
}

/// A failed store read, a missing or truncated file and a filter whose hash is not the row's
/// are errors, never misses.
#[test]
fn read_failures_are_errors_not_misses() {
    let run = replay(None, None);
    let index = &run.rec.index;
    let ancestry = Branches(index);
    let tip = *tips(index).iter().max_by_key(|b| b.height).unwrap();
    let b = index.path(tip)[tip.height as usize / 2];

    let r = reader(&run);
    let db = Faulty {
        db: Arc::clone(&run.db),
        fail: AtomicBool::new(true),
    };
    let failing = StoredFilters::new(Arc::new(db), run.files(), Arc::new(Synced));
    assert!(matches!(failing.filter(&b), Err(IndexError::Io(_))));
    assert!(matches!(failing.filter_header(&b), Err(IndexError::Io(_))));
    assert!(matches!(failing.filter_range(0, &tip, &ancestry), Err(IndexError::Io(_))));
    assert!(matches!(failing.filter_hash_range(0, &tip, &ancestry), Err(IndexError::Io(_))));

    let (entry, _) = run.value(&b).unwrap();
    let path = run.files().path(entry.pos.file as u32);
    let original = run.fs.bytes(&path).unwrap();
    let at = entry.pos.pos as usize + 32 + 1;
    let mut flipped = original.clone();
    flipped[at] ^= 1;
    run.fs.files.lock().unwrap().insert(path.clone(), flipped);
    assert!(matches!(r.filter(&b), Err(IndexError::Corrupt(_))), "a checksum mismatch");
    assert!(matches!(r.filter_range(0, &tip, &ancestry), Err(IndexError::Corrupt(_))));
    assert!(r.filter_header(&b).unwrap().is_some(), "headers need no file read");

    run.fs.files.lock().unwrap().insert(path.clone(), original[..at].to_vec());
    assert!(matches!(r.filter(&b), Err(IndexError::Io(_))), "a truncated file");
    run.fs.files.lock().unwrap().remove(&path);
    assert!(matches!(r.filter(&b), Err(IndexError::Io(_))), "no file");
    assert!(matches!(r.filter_range(0, &tip, &ancestry), Err(IndexError::Io(_))));
    assert!(r.filter_hash_range(0, &tip, &ancestry).unwrap().is_some());
}

/// The `dashd-v24-cfcheckpt` chain: its blocks are coinbase-only and every coinbase pays one
/// script, so each block's filter follows from its hash. The index's rows and files for all 2002
/// blocks, written as the data object writes them, read back as dashd's headers and filters at
/// heights 999, 1000, 1001, 1999, 2000 and 2001.
#[test]
fn cfcheckpt_boundary_headers_equal_dashd() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let chain = json(&root.join("dashd-v24-cfcheckpt/chain.json"));
    let checkpoints = json(&root.join("dashd-v24-cfcheckpt/checkpoints.json"));
    let genesis_hash: BlockHash = chain["phases"][0]["tip"].as_str().unwrap().parse().unwrap();
    let mut hashes = vec![genesis_hash];
    for e in chain["events"].as_array().unwrap() {
        hashes.push(e["connect"].as_str().unwrap().parse().unwrap());
    }
    assert_eq!(hashes.len(), 2002);

    let coinbases: Vec<&Value> = chain["phases"].as_array().unwrap()[1..]
        .iter()
        .map(|phase| &phase["blocks"][0]["transactions"][0]["common_result"])
        .collect();
    let scripts: HashSet<Vec<&Value>> = coinbases
        .iter()
        .map(|tx| {
            tx["vout"].as_array().unwrap().iter().map(|o| &o["scriptPubKey"]["hex"]).collect()
        })
        .collect();
    assert_eq!((coinbases.len(), scripts.len()), (6, 1), "one payout script");
    let bytes = hex(coinbases[0]["hex"].as_str().unwrap());
    let coinbase: Transaction =
        decode_tx(&mut bytes.as_slice()).unwrap().into_value().raw().clone();
    let g = json(&root.join("dashd-v24/blocks.json"));
    let genesis = g.as_array().unwrap().iter().find(|r| r["hash"] == genesis_hash.to_string());
    let genesis =
        hex(genesis.expect("g shares the genesis")["getblockfilter"]["filter"].as_str().unwrap());

    let fs = Arc::new(MemFs::default());
    let dir = PathBuf::from(DIR);
    let mut files = FilterFiles::new(fs.clone(), dir.clone());
    let engine = MemoryEngine::new(&BLOCKFILTER_DB);
    let mut batch = engine.batch();
    let (mut header, mut next) = (FilterHeader::from_lendian([0; 32]), FilePos::default());
    let mut refs = Vec::new();
    for (height, hash) in hashes.iter().enumerate() {
        let filter = match height {
            0 => BlockFilter::from_encoded(hash, genesis.clone(), false).unwrap(),
            _ => BlockFilter::build(hash, [&coinbase], |_| Err::<&[u8], _>(())).unwrap(),
        };
        header = filter.header(&header);
        let bytes = encode_entry(hash, filter.encoded());
        let at = files.start(next, bytes.len() as u64);
        let append = FileAppend {
            file: at.file,
            offset: at.offset,
            bytes,
        };
        next = append.end();
        files.append(&append).unwrap();
        let value = HeightValue {
            block_hash: *hash,
            filter: DbVal {
                filter_hash: filter.hash(),
                header,
                pos: FilterPos {
                    file: at.file as i32,
                    pos: at.offset,
                },
            },
        };
        batch.put(HEIGHT, &height_key(height as u32, &[]), &value.encode().unwrap());
        refs.push(BlockRef {
            id: BlockId(height as u32),
            hash: *hash,
            height: height as u32,
        });
    }
    engine.write(batch, Commit::Durable).unwrap();
    files.sync().unwrap();

    struct Active(Vec<BlockRef>);
    impl Ancestry for Active {
        fn ancestor(&self, _: &BlockRef, height: u32) -> Option<BlockRef> {
            self.0.get(height as usize).copied()
        }
    }
    let r = StoredFilters::new(Arc::new(engine), FilterFiles::new(fs, dir), Arc::new(Synced));
    let active = Active(refs.clone());
    let all = r.filter_hash_range(0, &refs[2001], &active).unwrap().unwrap();
    let headers = extend_headers(&FilterHeader::from_lendian([0; 32]), &all);
    let mut checked = Vec::new();
    for row in checkpoints.as_array().unwrap() {
        let height = row["height"].as_u64().unwrap() as usize;
        let b = &refs[height];
        assert_eq!(b.hash.to_string(), row["hash"].as_str().unwrap());
        let want = &row["getblockfilter"];
        let header = r.filter_header(b).unwrap().unwrap();
        assert_eq!(header.to_string(), want["header"].as_str().unwrap(), "header at {height}");
        assert_eq!(headers[height], header, "the hash range chains to it at {height}");
        let filter = r.filter(b).unwrap().unwrap();
        assert_eq!(filter.encoded(), hex(want["filter"].as_str().unwrap()), "filter at {height}");
        let range = r.filter_range(height as u32 - 1, b, &active).unwrap().unwrap();
        assert_eq!(range.last(), Some(&filter));
        checked.push(height);
    }
    assert_eq!(checked, [999, 1000, 1001, 1999, 2000, 2001]);
}
