// SPDX-License-Identifier: MIT

//! The coin statistics index through CH2's crash model (`dashd_chainstate::crash`): the real data
//! object and driver as the `coinstats-` subject on the chainstate SUT's synthetic blocks, every
//! crash point of the forward run, both crash kinds.
//!
//! At each boot, R11c checks the store at L: every height row on L's path holds an uncrashed
//! run's entry, `M` restores (through the data object's own `CustomInit`) to the uncrashed state
//! at L, and no height row survives above L. Every archive row holds the uncrashed entry for its
//! block, every block a durable locator of the run ever covered and L's path no longer holds has
//! one, and the reader answers each of them and each block on L's path as the uncrashed run
//! would. After the catch-up, the same holds at the new L, and the driver's reads of every block
//! up to V's tip equal the uncrashed run's, so the state decoded at L carried on exactly.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};

use dash_chain::BlockRef;
use dash_consensus::Block;
use dash_consensus::BlockId;
use dash_consensus::coins::BlockUndoRef;
use dash_consensus::coins::stats::CoinStatsAccumulator;
use dash_primitives::BlockHash;
use dashd_chainstate::crash::{
    ChainConfig, ChainSut, CrashReader, CrashSubject, NoFiles, SUB_STORE, SubConfig, SubMode, ops,
    synth,
};
use dashd_chainstate::subscriber::{BlockSource, SubscriberData, SubscriberError, height_key};
use dashd_indexes::coinstats::data::{
    ARCHIVE, COINSTATS_DB, COINSTATS_ROWS, HEIGHT, META, MUHASH, decode_height_value, restore_state,
};
use dashd_indexes::coinstats::reader::StoredCoinStats;
use dashd_indexes::coinstats::{CoinStatsAdapter, CoinStatsData, CoinStatsState};
use dashd_indexes::{
    CoinStatsEntry, CoinStatsReader, IndexError, IndexStatus, IndexSummary, SubsidyInputs,
};
use dashd_store::faulty::{
    CrashKind, CrashWindow, FaultyEngine, LocatorKind, Mark, Mode, Scenario, Trace, TraceEntry,
    World, explore, forward_trace,
};
use dashd_store::{Cf, DbSchema, KvRead};

struct CoinStatsSubject;

static SUBJECT: CoinStatsSubject = CoinStatsSubject;

/// The subsidy the synthetic chain claims at each height: below its coinbases' values from
/// height 1, so the unclaimed amount goes negative as an asset unlock's does.
struct SynthSubsidies;

fn subsidy(height: u32) -> i64 {
    40_000 + i64::from(height % 3) * 10_000
}

impl SubsidyInputs for SynthSubsidies {
    fn subsidy(&self, block: &BlockRef) -> Result<i64, IndexError> {
        Ok(subsidy(block.height))
    }
}

fn synth_block(b: &BlockRef) -> Result<Arc<synth::GenBlock>, String> {
    synth::lookup(&b.hash).ok_or_else(|| format!("no synthetic block {}", b.hash))
}

/// The synthetic blocks' data and undo data, as the chainstate SUT generated them.
struct SynthBlocks;

impl BlockSource for SynthBlocks {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        let g = synth_block(b).map_err(SubscriberError::Blocks)?;
        Ok(Arc::new(Block::new(*g.block.header(), g.block.txs().to_vec())))
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Ok(synth_block(b).map_err(SubscriberError::Blocks)?.undo.as_slice().to_vec())
    }
}

/// An uncrashed run's state after each block, by hash.
static REFERENCE: LazyLock<Mutex<HashMap<BlockHash, CoinStatsAccumulator>>> =
    LazyLock::new(Default::default);

/// The uncrashed run's state after each block of `path` (genesis first).
fn reference(path: &[BlockRef]) -> Result<Vec<CoinStatsAccumulator>, String> {
    let adapter = CoinStatsAdapter::new(Arc::new(SynthSubsidies));
    let mut cache = REFERENCE.lock().expect("unpoisoned");
    let mut out: Vec<CoinStatsAccumulator> = Vec::with_capacity(path.len());
    for b in path {
        if let Some(s) = cache.get(&b.hash) {
            out.push(s.clone());
            continue;
        }
        let g = synth_block(b)?;
        let mut s = out.last().cloned().unwrap_or_default();
        let undo = match b.height {
            0 => BlockUndoRef::default(),
            _ => BlockUndoRef::decode(&mut g.undo.as_slice()).map_err(|e| e.to_string())?,
        };
        adapter.append_block(&mut s, b, &g.block, &undo).map_err(|e| e.to_string())?;
        cache.insert(b.hash, s.clone());
        out.push(s);
    }
    Ok(out)
}

/// The uncrashed entries by hash: finalizing a MuHash is the checks' main cost.
static ENTRIES: LazyLock<Mutex<HashMap<BlockHash, CoinStatsEntry>>> =
    LazyLock::new(Default::default);

/// The entry of `b`, whose state is `s`.
fn entry(b: &BlockRef, s: &CoinStatsAccumulator) -> CoinStatsEntry {
    let mut cache = ENTRIES.lock().expect("unpoisoned");
    cache.entry(b.hash).or_insert_with(|| CoinStatsEntry::from_accumulator(&mut s.clone())).clone()
}

/// The uncrashed entry of the synthetic block `hash`.
fn entry_of(hash: &BlockHash) -> Result<CoinStatsEntry, String> {
    if let Some(e) = ENTRIES.lock().expect("unpoisoned").get(hash) {
        return Ok(e.clone());
    }
    let path = synth_path(hash)?;
    let want = reference(&path)?;
    Ok(entry(path.last().expect("genesis"), want.last().expect("genesis")))
}

/// The first height row on `path` that does not hold the uncrashed run's entry for its block.
fn check_rows(
    path: &[BlockRef],
    read: impl Fn(u32) -> Result<Option<Vec<u8>>, String>,
) -> Option<String> {
    let want = match reference(path) {
        Ok(want) => want,
        Err(e) => return Some(e),
    };
    path.iter().zip(&want).find_map(|(b, s)| {
        let row = match read(b.height) {
            Ok(Some(row)) => row,
            got => return Some(format!("the height row at {}: {got:?}", b.height)),
        };
        match decode_height_value(&row) {
            Ok((hash, got)) if hash == b.hash && got == entry(b, s) => None,
            got => Some(format!("the height row at {}: {got:?}", b.height)),
        }
    })
}

fn raw_row(raw: &FaultyEngine, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
    raw.get(cf, key).map_err(|e| format!("a read: {e}"))
}

/// Genesis to the synthetic block `hash`.
fn synth_path(hash: &BlockHash) -> Result<Vec<BlockRef>, String> {
    let mut path = Vec::new();
    let mut at = *hash;
    loop {
        let g = synth::lookup(&at).ok_or_else(|| format!("no synthetic block {at}"))?;
        path.push(BlockRef {
            id: BlockId(g.height),
            hash: at,
            height: g.height,
        });
        if g.height == 0 {
            path.reverse();
            return Ok(path);
        }
        at = g.parent;
    }
}

thread_local! {
    /// The run's world, from its last open: its trace has the run's durable locators.
    static WORLD: RefCell<Option<World>> = const { RefCell::new(None) };
}

/// Every block on the path to a locator the run's driver made durable, across its crashes.
fn covered() -> Result<HashSet<BlockHash>, String> {
    let trace = WORLD.with(|w| w.borrow().as_ref().map(World::trace)).ok_or("no world")?;
    let mut out = HashSet::new();
    for tip in locators(&trace).map(|(_, tip)| tip) {
        if out.insert(tip) {
            out.extend(synth_path(&tip)?.iter().map(|b| b.hash));
        }
    }
    Ok(out)
}

/// The trace's locator commits, in order.
fn locators(trace: &Trace) -> impl Iterator<Item = (LocatorKind, BlockHash)> + '_ {
    trace.marks().filter_map(|m| match m {
        Mark::Locator {
            store,
            tip,
            kind,
        } if *store == SUB_STORE => Some((*kind, BlockHash::from(*tip))),
        _ => None,
    })
}

/// The store as the reader reads it.
struct RawRead<'a>(&'a FaultyEngine);

impl KvRead for RawRead<'_> {
    fn get(&self, cf: Cf, key: &[u8]) -> dashd_store::Result<Option<Vec<u8>>> {
        self.0.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> dashd_store::Result<Vec<Option<Vec<u8>>>> {
        self.0.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = dashd_store::Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.0.iter_prefix(cf, prefix)
    }
}

struct Synced;

impl IndexStatus for Synced {
    fn summary(&self) -> IndexSummary {
        IndexSummary {
            name: "coinstatsindex",
            synced: true,
            best_block_height: 0,
        }
    }

    fn block_until_synced(&self) -> bool {
        true
    }
}

/// Every archive row holds the uncrashed entry for its block; every block a durable locator
/// covered and `path` (genesis to L) no longer holds is archived; the reader answers those and
/// `path`'s blocks with their uncrashed entries.
fn check_archive(raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
    let mut archived = Vec::new();
    for row in raw.iter_prefix(ARCHIVE, &[]) {
        let (k, v) = match row {
            Ok(row) => row,
            Err(e) => return Some(format!("a read: {e}")),
        };
        let hash = match archive_hash(&k) {
            Ok(b) => b,
            Err(e) => return Some(e),
        };
        let want = match entry_of(&hash) {
            Ok(want) => want,
            Err(e) => return Some(e),
        };
        match decode_height_value(&[k.as_slice(), &v].concat()) {
            Ok((_, got)) if got == want => archived.push(hash),
            got => return Some(format!("the archive row of {hash}: {got:?}")),
        }
    }
    let on_path: HashSet<BlockHash> = path.iter().map(|b| b.hash).collect();
    let stale = match covered() {
        Ok(c) => c.into_iter().filter(|h| !on_path.contains(h)),
        Err(e) => return Some(e),
    };
    let reader = StoredCoinStats::new(Arc::new(RawRead(raw)), Arc::new(Synced));
    let mut blocks: Vec<BlockHash> = stale.chain(archived).collect();
    blocks.extend(path.iter().map(|b| b.hash));
    blocks.sort();
    blocks.dedup();
    blocks.into_iter().find_map(|hash| {
        let (b, want) = match synth_path(&hash).and_then(|p| Ok((p[p.len() - 1], entry_of(&hash)?)))
        {
            Ok(w) => w,
            Err(e) => return Some(e),
        };
        match reader.look_up_stats(&b) {
            Ok(Some(got)) if got == want => None,
            got => Some(format!("the reader's answer for {hash} at {}: {got:?}", b.height)),
        }
    })
}

fn archive_hash(key: &[u8]) -> Result<BlockHash, String> {
    let bytes: [u8; 32] = key.try_into().map_err(|_| format!("an archive key of {}", key.len()))?;
    Ok(BlockHash::from(bytes))
}

impl CrashSubject for CoinStatsSubject {
    type State = CoinStatsState;
    type Files = NoFiles;

    fn prefix(&self) -> &'static str {
        "coinstats-"
    }
    fn name(&self) -> &'static str {
        "coinstatsindex"
    }
    fn locator(&self) -> (Cf, &'static [u8]) {
        COINSTATS_ROWS.locator
    }
    fn db(&self) -> &'static DbSchema {
        &COINSTATS_DB
    }
    fn data(
        &self,
        world: &World,
    ) -> Box<dyn SubscriberData<E = FaultyEngine, State = CoinStatsState>> {
        WORLD.with(|w| *w.borrow_mut() = Some(world.clone()));
        Box::new(CoinStatsData::<FaultyEngine>::new(Arc::new(SynthSubsidies)))
    }
    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::new(SynthBlocks)
    }
    /// Every height row up to L, `M` restored with the row at L to the state at L, and the
    /// archive and the reader's answers.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        let l = path.last()?;
        if let Some(e) = check_rows(path, |h| raw_row(raw, HEIGHT, &height_key(h, &[]))) {
            return Some(e);
        }
        let m = match raw_row(raw, META, MUHASH) {
            Ok(m) => m,
            Err(e) => return Some(e),
        };
        let state = match restore_state(m.as_deref(), Some(l), |h| {
            raw_row(raw, HEIGHT, &height_key(h, &[])).map_err(SubscriberError::Data)
        }) {
            Ok(state) => state,
            Err(e) => return Some(format!("the state at L = {}: {e}", l.height)),
        };
        let want = match reference(path) {
            Ok(mut want) => want.pop().expect("L"),
            Err(e) => return Some(e),
        };
        if state.height != Some(l.height)
            || state.accumulator.muhash().to_bytes() != want.muhash().to_bytes()
            || state.accumulator.totals() != want.totals()
        {
            return Some(format!(
                "M restores the state at {:?}, not at L = {}",
                state.height, l.height
            ));
        }
        check_archive(raw, path)
    }
    /// No height row above L after the open's blind rewind.
    fn check_rewound(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        let l = path.last()?.height;
        raw.iter_prefix(HEIGHT, &[]).find_map(|row| match row {
            Ok((k, _)) => {
                let h = u32::from_be_bytes(k[..4].try_into().expect("4 bytes"));
                (h > l).then(|| format!("a height row at {h} above L = {l}"))
            }
            Err(e) => Some(format!("a read: {e}")),
        })
    }
    /// The driver's reads of every block up to V's tip equal the uncrashed run's.
    fn check_tip(&self, reader: &dyn CrashReader, path: &[BlockRef]) -> Option<String> {
        check_rows(path, |h| reader.read(HEIGHT, &height_key(h, &[])).map_err(|e| e.to_string()))
    }
}

fn scenario(name: &'static str, mode: SubMode, operations: &str) -> Scenario<ChainSut> {
    Scenario::new(
        name,
        ChainConfig {
            index: Some(SubConfig::with_subject(&SUBJECT, mode, 96)),
            ..ChainConfig::default()
        },
        ops(operations),
    )
}

/// The exhaustive sweep: a clean forward run that commits the locator kinds `kinds`, then every
/// crash point with both crash kinds.
fn sweep(sc: &Scenario<ChainSut>, kinds: &[LocatorKind]) {
    assert_eq!(sc.window, CrashWindow::all());
    let (trace, violations) = forward_trace(sc);
    assert!(violations.is_empty(), "{}: forward: {violations:?}", sc.name);
    let written: HashSet<LocatorKind> = locators(&trace).map(|(kind, _)| kind).collect();
    for kind in kinds {
        assert!(written.contains(kind), "{}: no {kind:?} locator in {written:?}", sc.name);
    }
    let subscriber_events = trace
        .entries
        .iter()
        .filter(|e| matches!(e, TraceEntry::Event { store, .. } if store == SUB_STORE))
        .count();
    assert!(subscriber_events > 0, "{}: the subscriber wrote nothing", sc.name);
    let report = explore(
        sc,
        Mode::Exhaustive {
            max_branches_per_image: 4096,
        },
    );
    eprintln!("{}: {}", sc.name, report.summary());
    assert!(report.is_clean(), "{}: {}", sc.name, report.summary());
    assert_eq!(report.truncated, 0);
    let events = trace.entries.iter().filter(|e| matches!(e, TraceEntry::Event { .. })).count();
    for kind in [CrashKind::ProcessKill, CrashKind::PowerLoss] {
        let images = report.per_image.iter().filter(|(_, k, ..)| *k == kind).count();
        assert_eq!(images, events, "{}: every crash point for {kind:?}", sc.name);
    }
}

/// Live from genesis: commits at each flush, a reorg above L (archived from the journal).
#[test]
fn coinstats_synced_crash_sweep() {
    sweep(
        &scenario(
            "coinstats-synced",
            SubMode::Synced {
                backlog: 0,
            },
            "c2;f;c2;r2:3;f",
        ),
        &[LocatorKind::CommitStep],
    );
}

/// A reorg below L: the rewind commit reverses (F, L] through the data object, archives it and
/// writes `M` at F; a backlog keeps events queued across the flushes.
#[test]
fn coinstats_synced_crash_sweep_rewinds_below_l() {
    sweep(
        &scenario(
            "coinstats-rewind",
            SubMode::Synced {
                backlog: 2,
            },
            "c4;f;r2:3;f;c1;f",
        ),
        &[LocatorKind::Rewind],
    );
}

/// A catch-up whose one cadence commit (at h5, with progress at h8) takes `M` from a journal
/// entry behind progress; the timer and the handoff's capped commits stay at that cap, and the
/// reorg comes after the handoff, its disconnects above L.
#[test]
fn coinstats_syncing_crash_sweep() {
    sweep(
        &scenario(
            "coinstats-syncing",
            SubMode::Syncing {
                cadence: 8,
            },
            "c5;f;c5;s11;k;s2;r8:3;s20;f",
        ),
        &[LocatorKind::Cadence],
    );
}

/// A reorg below L during the catch-up: the Syncing driver disconnects from h10 to h3 under
/// L = h5, so the rewind commit reverses (3, 5] from the state at L before the handoff.
#[test]
fn coinstats_syncing_crash_sweep_rewinds_below_l() {
    let sc = scenario(
        "coinstats-syncing-rewind",
        SubMode::Syncing {
            cadence: 8,
        },
        "c5;f;c5;s11;k;r3:4;s20;f",
    );
    let (trace, _) = forward_trace(&sc);
    let rewind = trace.marks().position(|m| {
        matches!(m, Mark::Locator { kind: LocatorKind::Rewind, store, .. } if *store == SUB_STORE)
    });
    let synced = trace.marks().position(
        |m| matches!(m, Mark::SubscriberState { synced: true, store, .. } if *store == SUB_STORE),
    );
    assert!(rewind < synced && rewind.is_some(), "a rewind while Syncing: {rewind:?} {synced:?}");
    sweep(&sc, &[LocatorKind::Cadence, LocatorKind::Rewind]);
}
