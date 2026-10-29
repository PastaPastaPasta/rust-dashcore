//! End-to-end blk-file mode on a synthetic regtest-style chain: genesis <- b1 <- b2, a stale
//! sibling of b2, a duplicate copy of b1 and an orphan, written out of order. Then the records
//! not linked to genesis, classified against a mock of dashd's block index, one per class.
//!
//! The blocks are synthetic (mined here against regtest's PoW limit), not dashd output: the
//! tests are about how the harness indexes and classifies records, which no real chain
//! exercises on demand. The mock answers as dashd's `getblockheader` and `getblockhash` do
//! (`tests/block_index.rs` checks `BlockIndex for Rpc` against real dashd replies).

mod common;

use common::block;
use dash_conformance::check::{Context, H256};
use dash_conformance::report::{Identity, Meta, Reporter, Summary};
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use dash_conformance::blkfile::Record;
use dash_conformance::run::{
    BlkOptions, BlockIndex, Classifier, IndexedHeader, RunResult, Unlinked, run_blk,
};
use dashcore::block::Block;
use dashcore::consensus::serialize;
use dashcore::hashes::Hash;
use dashcore::{BlockHash, Network};

fn record(file: &mut Vec<u8>, b: &Block) {
    let bytes = serialize(b);
    file.extend_from_slice(&Network::Regtest.magic().to_le_bytes());
    file.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    file.extend_from_slice(&bytes);
}

fn hash(b: &Block) -> H256 {
    b.block_hash().to_byte_array()
}

fn write_chain(dir: &std::path::Path, blocks: &[&Block]) {
    let mut file = Vec::new();
    for b in blocks {
        record(&mut file, b);
    }
    file.extend_from_slice(&[0; 1024]);
    std::fs::write(dir.join("blk00000.dat"), &file).unwrap();
}

struct Setup {
    blocks: tempfile::TempDir,
    out: tempfile::TempDir,
    meta: Meta,
    ctx: Context,
    opts: BlkOptions,
}

impl Setup {
    fn run(&self, opts: &BlkOptions) -> (RunResult, Summary, Summary) {
        self.run_with(opts, None)
    }

    fn run_with(
        &self,
        opts: &BlkOptions,
        index: Option<&dyn BlockIndex>,
    ) -> (RunResult, Summary, Summary) {
        let reporter =
            Reporter::open(self.out.path(), &self.meta, &Identity::of_build(opts.policy()), 10, 0)
                .unwrap();
        let result = run_blk(&self.ctx, opts, index, &reporter).unwrap();
        let summary = reporter.write_summary().unwrap();
        let gate = reporter.gate(&result.scope).unwrap();
        (result, summary, gate)
    }
}

/// genesis <- b1 <- b2, a stale sibling of b2, an orphan and a duplicate of b1.
fn setup() -> (Setup, [Block; 5]) {
    let g = block(BlockHash::all_zeros(), 0);
    let b1 = block(g.block_hash(), 1);
    let b2 = block(b1.block_hash(), 2);
    let stale = block(b1.block_hash(), 3);
    let orphan = block(BlockHash::from_byte_array([7; 32]), 4);
    let setup = setup_files(&[&b2, &g, &stale, &b1, &orphan, &b1], hash(&g), hash(&b2));
    (setup, [g, b1, b2, stale, orphan])
}

/// A blocks directory holding `blocks` in that order, anchored at `anchor`.
fn setup_files(blocks_in_order: &[&Block], genesis: H256, anchor: H256) -> Setup {
    let blocks = tempfile::tempdir().unwrap();
    write_chain(blocks.path(), blocks_in_order);
    let meta = Meta {
        net: "regtest".into(),
        source: "blk".into(),
        chunk: None,
        blocks_dir: Some(blocks.path().display().to_string()),
    };
    let opts = BlkOptions {
        blocks_dir: blocks.path().to_path_buf(),
        magic: Network::Regtest.magic().to_le_bytes(),
        genesis: Some(genesis),
        from: 0,
        to: None,
        include_stale: true,
        resume: true,
        anchor: Some(anchor),
    };
    Setup {
        blocks,
        out: tempfile::tempdir().unwrap(),
        meta,
        ctx: Context {
            net: "regtest".into(),
            source: "blk",
        },
        opts,
    }
}

#[test]
fn blk_mode_end_to_end() {
    let (s, [g, b1, b2, stale, orphan]) = setup();
    let (result, summary, gate) = s.run(&s.opts);
    assert_eq!(result.failed_units, 0);
    assert_eq!(result.stats.blocks, 4, "genesis, b1, b2 and the stale sibling");
    assert_eq!(result.stats.stale_blocks, 1);
    assert_eq!(result.stats.consensus_blocks, 4, "the dash_consensus leg, the stale block too");
    assert_eq!(result.stats.oracle_hash_checks, 3, "best-chain blocks have a reference");
    let keys: Vec<&str> = summary.mismatches_by_key.keys().map(String::as_str).collect();
    assert_eq!(keys, ["blk.orphan:harness"], "{:#?}", summary.mismatches_by_key);
    let detail = &summary.mismatches_by_key["blk.orphan:harness"].examples[0].detail;
    assert!(detail.ends_with("no oracle to classify it"), "{detail}");
    assert_eq!(summary.mismatches_by_key["blk.orphan:harness"].count, 1, "duplicate is no orphan");
    assert_eq!((gate.failing, gate.missing.len()), (1, 0), "index findings count towards the gate");
    assert_eq!(gate.incomplete, 1, "an orphan means a block may not have been checked");
    assert_eq!(summary.unaccounted, 0);

    // Resuming checks nothing new, keeps the totals, and the gate still fails on the
    // finding (review r1 finding 1).
    let (again, summary2, gate2) = s.run(&s.opts);
    assert_eq!(again.stats.blocks, 0);
    assert_eq!(summary2.totals.blocks, 4);
    assert_eq!(summary2.mismatches_by_key["blk.orphan:harness"].count, 1, "index unit replaced");
    assert_eq!((gate2.failing, gate2.missing.len()), (1, 0));

    // A narrower request checks the whole file again (units are whole files), and asking for
    // heights the chain does not reach is reported.
    let narrow = BlkOptions {
        from: 1,
        to: Some(9),
        resume: false,
        ..s.opts.clone()
    };
    let (r, summary3, gate3) = s.run(&narrow);
    assert_eq!(r.stats.blocks, 4);
    assert_eq!(summary3.totals.blocks, 4);
    assert!(summary3.mismatches_by_key.contains_key("blk.range:harness"));
    assert_eq!(gate3.failing, 2, "orphan and range");

    // Another comparison policy reuses nothing.
    let best_only = BlkOptions {
        include_stale: false,
        ..s.opts.clone()
    };
    let (r, summary4, _) = s.run(&best_only);
    assert_eq!(r.stats.blocks, 3);
    assert_eq!(summary4.totals.blocks, 3, "policy runs are counted separately");

    // Same-size content replacement (b2 re-mined with another time) changes the file's
    // signature: the old checkpoint is invalidated and the file is re-checked.
    let b2x = {
        let mut b = b2.clone();
        b.header.time += 7;
        while b.header.validate_pow(b.header.target()).is_err() {
            b.header.nonce += 1;
        }
        b
    };
    assert_eq!(serialize(&b2x).len(), serialize(&b2).len());
    write_chain(s.blocks.path(), &[&b2x, &g, &stale, &b1, &orphan, &b1]);
    let moved = BlkOptions {
        anchor: Some(hash(&b2x)),
        ..s.opts.clone()
    };
    let (r, summary5, gate5) = s.run(&moved);
    assert_eq!(r.stats.blocks, 4, "re-checked");
    assert_eq!(summary5.totals.blocks, 4);
    assert_eq!(gate5.missing.len(), 0);
    let invalidated = std::fs::read_to_string(s.out.path().join("invalidated.ndjson")).unwrap();
    assert!(invalidated.contains("blk:0"), "{invalidated}");

    // An anchor that is not in the blk files stops the run; so does an inverted range.
    let bad_anchor = BlkOptions {
        anchor: Some([9; 32]),
        ..s.opts.clone()
    };
    let reporter =
        Reporter::open(s.out.path(), &s.meta, &Identity::of_build(bad_anchor.policy()), 10, 0)
            .unwrap();
    assert!(run_blk(&s.ctx, &bad_anchor, None, &reporter).is_err());
    let inverted = BlkOptions {
        from: 2,
        to: Some(1),
        ..s.opts.clone()
    };
    assert!(run_blk(&s.ctx, &inverted, None, &reporter).is_err());
}

/// dashd's block index as the mock oracle sees it.
#[derive(Default)]
struct MockIndex {
    headers: HashMap<H256, IndexedHeader>,
    active: HashMap<u32, H256>,
}

impl MockIndex {
    /// Adds `b` at `height`; `active` also puts it on the active chain, `data` marks its data
    /// as stored.
    fn add(&mut self, b: &Block, height: u32, active: bool, data: bool) {
        let h = hash(b);
        let header = IndexedHeader {
            height,
            prev: b.header.prev_blockhash.to_byte_array(),
            active,
            has_data: data,
        };
        self.headers.insert(h, header);
        if active {
            self.active.insert(height, h);
        }
    }

    fn get(&mut self, b: &Block) -> &mut IndexedHeader {
        self.headers.get_mut(&hash(b)).unwrap()
    }
}

impl BlockIndex for MockIndex {
    fn header(&self, hash: &H256) -> Result<Option<IndexedHeader>> {
        Ok(self.headers.get(hash).cloned())
    }

    fn active_at(&self, height: u32) -> Result<Option<H256>> {
        Ok(self.active.get(&height).copied())
    }
}

/// Where dashd's initial block download is when the records are classified. The blk files
/// were indexed with `main[5]` not yet written.
#[derive(Debug, Clone, Copy)]
enum Ibd {
    /// `main[5]` is still in flight: dashd has its header only, `main[6]` and `main[7]` wait.
    InFlight,
    /// `main[5]` arrived after the files were indexed; dashd has not connected it yet.
    Arrived,
    /// dashd has connected `main[5..=7]`.
    Connected,
}

/// The blocks of [`unlinked_setup`].
struct Blocks {
    /// The best chain, `main[0..=7]`; the files' best tip (and anchor) is `main[4]`.
    main: Vec<Block>,
    /// A fork off `main[1]` whose first block dashd has the header of only.
    fork: Vec<Block>,
    /// A fork off `main[3]` crossing the tip, likewise.
    high_fork: Vec<Block>,
}

/// dashd's view of [`Blocks`] at `ibd`, and blk files with every block dashd has data for
/// except `main[5]`: `main[6]` and `main[7]` were downloaded ahead of it. `fork[1]` and
/// `fork[2]` hang off `fork[0]`, and `high_fork[1]` off `high_fork[0]`, neither of which
/// dashd downloaded.
fn unlinked_setup(ibd: Ibd) -> (Setup, MockIndex, Blocks) {
    let mut main = vec![block(BlockHash::all_zeros(), 0)];
    for tag in 1..=7 {
        main.push(block(main.last().unwrap().block_hash(), tag));
    }
    let mut fork = vec![block(main[1].block_hash(), 20)];
    for tag in 21..=22 {
        fork.push(block(fork.last().unwrap().block_hash(), tag));
    }
    let high_fork0 = block(main[3].block_hash(), 50);
    let high_fork = vec![high_fork0.clone(), block(high_fork0.block_hash(), 51)];
    let mut index = MockIndex::default();
    for (h, b) in main.iter().enumerate() {
        let connected = h <= 4 || matches!(ibd, Ibd::Connected);
        let data = connected || h != 5 || matches!(ibd, Ibd::Arrived);
        index.add(b, h as u32, connected, data);
    }
    for (b, h, data) in [(&fork[0], 2, false), (&fork[1], 3, true), (&fork[2], 4, true)] {
        index.add(b, h, false, data);
    }
    index.add(&high_fork[0], 4, false, false);
    index.add(&high_fork[1], 5, false, true);
    let files: Vec<&Block> = [&main[7], &fork[2], &main[6]]
        .into_iter()
        .chain(&main[..5])
        .chain([&fork[1], &high_fork[1]])
        .collect();
    let s = setup_files(&files, hash(&main[0]), hash(&main[4]));
    (
        s,
        index,
        Blocks {
            main,
            fork,
            high_fork,
        },
    )
}

fn by_check<'a>(summary: &'a Summary, key: &str) -> Vec<(Option<u32>, &'a str, &'a str)> {
    let mut v: Vec<_> = summary
        .mismatches_by_key
        .get(key)
        .map(|g| &g.examples[..])
        .unwrap_or_default()
        .iter()
        .map(|m| (m.height, m.block_hash.as_deref().unwrap_or(""), m.detail.as_str()))
        .collect();
    v.sort();
    v
}

fn display(b: &Block) -> String {
    b.block_hash().to_string()
}

/// Blocks downloaded ahead of their parent (at every stage of dashd's download), and stale
/// branches whose base dashd never stored, are informational: the gate passes, and the stale
/// blocks at or below the tip are checked at dashd's heights.
#[test]
fn unlinked_records_dashd_also_leaves_off_its_active_chain_are_informational() {
    for ibd in [Ibd::InFlight, Ibd::Arrived, Ibd::Connected] {
        let (s, index, u) = unlinked_setup(ibd);
        let (result, summary, gate) = s.run_with(&s.opts, Some(&index));
        assert_eq!(result.failed_units, 0);
        let counts = (gate.failing, gate.incomplete, gate.missing.len());
        assert_eq!(counts, (0, 0, 0), "{ibd:?}: {summary:#?}");
        assert_eq!(gate.totals.informational, 5);
        let keys: Vec<&str> = summary.mismatches_by_key.keys().map(String::as_str).collect();
        assert_eq!(keys, ["blk.unlinked_ahead:harness", "blk.unlinked_stale:harness"]);

        let ahead = by_check(&summary, "blk.unlinked_ahead:harness");
        let want = [(6, &u.main[6]), (7, &u.main[7])];
        assert_eq!(ahead.len(), 2);
        for ((h, hash, detail), (want_h, b)) in ahead.iter().zip(want) {
            assert_eq!((*h, *hash), (Some(want_h), display(b).as_str()));
            assert!(detail.contains("descendant of the best tip 4"), "{detail}");
        }
        let stale = by_check(&summary, "blk.unlinked_stale:harness");
        let want = [(3, &u.fork[1], true), (4, &u.fork[2], true), (5, &u.high_fork[1], false)];
        assert_eq!(stale.len(), 3);
        for ((h, hash, detail), (want_h, b, checked)) in stale.iter().zip(want) {
            assert_eq!((*h, *hash), (Some(want_h), display(b).as_str()));
            let note = if checked {
                "checked as a stale block at that height"
            } else {
                "not checked"
            };
            assert!(detail.contains(note), "{detail}");
        }

        // main[0..=4] plus the two stale blocks at or below the tip, all against a reference
        // hash.
        assert_eq!(result.stats.blocks, 7);
        assert_eq!(result.stats.stale_blocks, 2);
        assert_eq!(result.stats.oracle_hash_checks, 7);
        assert_eq!(result.stats.max_height, Some(4));
    }

    // Without --include-stale the stale ones are still informational, but not checked.
    let (s, index, _) = unlinked_setup(Ibd::InFlight);
    let best_only = BlkOptions {
        include_stale: false,
        ..s.opts.clone()
    };
    let (r, summary, gate) = s.run_with(&best_only, Some(&index));
    assert_eq!((r.stats.blocks, r.stats.stale_blocks), (5, 0));
    assert_eq!((gate.failing, gate.incomplete, gate.totals.informational), (0, 0, 5));
    for (_, _, detail) in by_check(&summary, "blk.unlinked_stale:harness") {
        assert!(detail.contains("not checked"), "{detail}");
    }
}

/// A resumed run whose classification of a checked stale block changed re-checks its file.
#[test]
fn a_changed_classification_rechecks_the_file() {
    let (s, mut index, u) = unlinked_setup(Ibd::Connected);
    let (r, _, gate) = s.run_with(&s.opts, Some(&index));
    assert_eq!((r.stats.blocks, gate.failing), (7, 0));
    let (r, _, _) = s.run_with(&s.opts, Some(&index));
    assert_eq!(r.stats.blocks, 0, "nothing changed");

    // dashd now has fork[0]'s data, which no record holds: fork[1] fails and loses its height.
    index.get(&u.fork[0]).has_data = true;
    let (r, summary, gate) = s.run_with(&s.opts, Some(&index));
    assert_eq!(r.stats.blocks, 6, "re-checked without fork[1]");
    let invalidated = std::fs::read_to_string(s.out.path().join("invalidated.ndjson")).unwrap();
    assert!(invalidated.contains("blk:0"), "{invalidated}");
    let orphans = by_check(&summary, "blk.orphan:harness");
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].1, display(&u.fork[1]));
    assert_eq!((gate.failing, gate.incomplete, gate.totals.informational), (1, 1, 4));
}

/// Every unlinked record dashd's index does not explain stays a failing `blk.orphan`.
#[test]
fn unexplained_unlinked_records_fail_the_gate() {
    let (_, mut index, u) = unlinked_setup(Ibd::Connected);
    let main = &u.main;
    // dashd does not know it (both libraries would hash it wrongly, or it is foreign).
    let unknown = block(BlockHash::from_byte_array([7; 32]), 30);
    // dashd knows it with another parent than the bytes say.
    let other_parent = block(BlockHash::from_byte_array([8; 32]), 31);
    index.add(&other_parent, 3, false, true);
    index.get(&other_parent).prev = hash(&main[2]);
    // On dashd's active chain at or below the tip (the mock says so without moving main[3]
    // off the active chain): the files should link it.
    let below_tip = block(BlockHash::from_byte_array([9; 32]), 32);
    index.add(&below_tip, 3, false, true);
    index.get(&below_tip).active = true;
    // Stale, but dashd has its parent's data, which no record hashes to.
    let parent_with_data = block(main[2].block_hash(), 33);
    let lost_parent = block(parent_with_data.block_hash(), 34);
    index.add(&parent_with_data, 3, false, true);
    index.add(&lost_parent, 4, false, true);
    let files: Vec<&Block> = main[..5]
        .iter()
        .chain([&main[6], &u.fork[1], &unknown, &other_parent, &below_tip, &lost_parent])
        .collect();
    let s = setup_files(&files, hash(&main[0]), hash(&main[4]));

    let (result, summary, gate) = s.run_with(&s.opts, Some(&index));
    assert_eq!(result.failed_units, 0);
    let orphans = by_check(&summary, "blk.orphan:harness");
    let mut whys: Vec<&str> =
        orphans.iter().map(|(_, _, d)| d.split_once("): ").unwrap().1).collect();
    whys.sort();
    assert_eq!(
        whys,
        [
            "dashd does not know this block",
            "dashd has its parent's data, which no record in the files hashes to (written \
             after the files were indexed, or hashed wrongly by both libraries; re-run)",
            "dashd's active chain holds it at height 3, at or below the best tip 4",
            &format!("dashd's parent is {}", display(&main[2])),
        ]
    );
    assert_eq!((gate.failing, gate.incomplete), (4, 4), "{summary:#?}");
    assert_eq!(gate.totals.informational, 2, "main[6] and fork[1] are still expected");

    // dashd reorganised away from the files' tip after they were indexed: a block above it
    // no longer descends from it.
    for ibd in [Ibd::InFlight, Ibd::Connected] {
        let (s, mut index, u) = unlinked_setup(ibd);
        let replaced = block(u.main[3].block_hash(), 40);
        index.add(&replaced, 4, true, true);
        let main4 = index.get(&u.main[4]);
        main4.active = false;
        if matches!(ibd, Ibd::InFlight) {
            // main[5..] now hang off a block that is not the files' tip either.
            index.get(&u.main[5]).prev = hash(&replaced);
        }
        let (_, summary, gate) = s.run_with(&s.opts, Some(&index));
        let orphans = by_check(&summary, "blk.orphan:harness");
        match ibd {
            Ibd::Connected => {
                assert_eq!(orphans.len(), 2, "main[6] and main[7]: {summary:#?}");
                assert!(orphans.iter().all(|(_, _, d)| d.contains("reorganised; re-run")));
                assert_eq!((gate.failing, gate.totals.informational), (2, 3));
            }
            _ => {
                // Off the active chain, not descending from the tip, parent in flight: a
                // stale branch as far as dashd's index tells.
                assert!(orphans.is_empty(), "{orphans:?}");
                assert_eq!((gate.failing, gate.totals.informational), (0, 5));
                assert!(!summary.mismatches_by_key.contains_key("blk.unlinked_ahead:harness"));
            }
        }
    }
}

/// A record the libraries hash differently is never classified, whatever dashd says.
#[test]
fn records_the_libraries_disagree_on_are_unexplained() {
    let (_, index, u) = unlinked_setup(Ibd::Connected);
    let record = |dc: Option<&Block>, bs: Option<&Block>| Record {
        file: 0,
        offset: 8,
        len: 80,
        prev: u.main[6].header.prev_blockhash.to_byte_array(),
        dc_hash: dc.map(hash),
        bs_hash: bs.map(hash),
    };
    let mut c = Classifier::new(&index, (4, hash(&u.main[4])), HashSet::new());
    let agreed = c.classify(&record(Some(&u.main[6]), Some(&u.main[6]))).unwrap();
    assert_eq!(
        agreed,
        Unlinked::Ahead {
            height: 6
        }
    );
    for (dc, bs) in
        [(Some(&u.main[6]), Some(&u.main[7])), (None, Some(&u.main[6])), (Some(&u.main[6]), None)]
    {
        let class = c.classify(&record(dc, bs)).unwrap();
        assert_eq!(class, Unlinked::Unexplained("the libraries do not agree on its hash".into()));
    }
}
