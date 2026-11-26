// SPDX-License-Identifier: MIT

//! The transaction index through CH2's crash model (`dashd_chainstate::crash`): the real data
//! object and driver as the `txindex-` subject on the chainstate SUT's synthetic blocks, every
//! crash point of the forward run, both crash kinds.
//!
//! Every open checks that the rows a crash left all rank below the new epoch's first sequence
//! number (a failure panics the open, which the harness reports as R0; a failed epoch write
//! refuses the open), so every append after a restart outranks them (with the in-epoch
//! monotonicity that `tests/txindex.rs` pins). R11c checks the rows of every block up to L at the
//! boot and after the catch-up, and the driver's reads of every block up to V's tip.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_chain::{BlockRef, FlatFilePos};
use dash_consensus::Block;
use dash_consensus::primitives::compact_size_len;
use dash_primitives::BlockHash;
use dashd_chainstate::crash::{
    ChainConfig, ChainSut, CrashReader, CrashSubject, NoFiles, SUB_STORE, SubConfig, SubMode, ops,
    register_subject, synth,
};
use dashd_chainstate::subscriber::{BlockSource, SubscriberData, SubscriberError};
use dashd_indexes::txindex::codec::{self, TxKey, TxPos};
use dashd_indexes::txindex::data::SEQ_BITS;
use dashd_indexes::txindex::{EPOCH, META, TX, TXINDEX_DB, TXINDEX_ROWS, TxIndexData};
use dashd_indexes::{IndexError, TxPositions};
use dashd_store::faulty::{
    CommitKind, CrashKind, CrashWindow, FaultyEngine, LocatorKind, Mark, Mode, OpKind, Scenario,
    TraceEntry, World, explore, forward_trace,
};
use dashd_store::{Cf, DbSchema, KvRead};

struct TxIndexSubject;

static SUBJECT: TxIndexSubject = TxIndexSubject;

/// The synthetic blocks' data, as the chainstate SUT generated them.
struct SynthBlocks;

fn synth_block(b: &BlockRef) -> Result<Block, String> {
    let g = synth::lookup(&b.hash).ok_or_else(|| format!("no synthetic block {}", b.hash))?;
    Ok(Block::new(*g.block.header(), g.block.txs().to_vec()))
}

impl BlockSource for SynthBlocks {
    fn block(&self, b: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        synth_block(b).map(Arc::new).map_err(SubscriberError::Blocks)
    }

    fn undo(&self, b: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        Err(SubscriberError::Blocks(format!("txindex reads no undo ({})", b.hash)))
    }
}

/// A position per block hash: the model's block index carries none for subscribers.
struct SynthPositions;

fn position(hash: &BlockHash) -> FlatFilePos {
    FlatFilePos {
        file: 0,
        pos: u32::from_le_bytes(hash.as_bytes()[..4].try_into().expect("4 bytes")),
    }
}

impl TxPositions for SynthPositions {
    fn data_pos(&self, block: &BlockRef) -> Result<FlatFilePos, IndexError> {
        Ok(position(&block.hash))
    }
}

/// Every row's key and sequence number.
fn rows(raw: &FaultyEngine) -> Result<BTreeMap<Vec<u8>, u64>, String> {
    raw.iter_prefix(TX, &[])
        .map(|row| {
            let (key, value) = row.map_err(|e| format!("a row read: {e}"))?;
            TxKey::decode(&key).map_err(|e| format!("a row key: {e}"))?;
            let seq = codec::decode_seq(&value).map_err(|e| format!("a row value: {e}"))?;
            Ok((key, seq))
        })
        .collect()
}

fn epoch(raw: &FaultyEngine) -> Result<u32, String> {
    match raw.get(META, EPOCH) {
        Ok(Some(v)) => codec::decode_epoch(&v).map_err(|e| e.to_string()),
        other => Err(format!("the epoch row: {other:?}")),
    }
}

/// The keys of b's transactions (none for genesis).
fn keys(b: &BlockRef) -> Result<Vec<Vec<u8>>, String> {
    if b.height == 0 {
        return Ok(Vec::new());
    }
    let block = synth_block(b)?;
    let mut tx_offset = compact_size_len(block.txs().len() as u64) as u32;
    block
        .txs()
        .iter()
        .map(|tx| {
            let key = TxKey {
                prefix: codec::prefix(&tx.txid()),
                pos: TxPos {
                    block: position(&b.hash),
                    tx_offset,
                },
            };
            tx_offset += tx.size();
            key.encode().map(|k| k.to_vec()).map_err(|e| e.to_string())
        })
        .collect()
}

/// A row that would not rank below `floor`, the first sequence number of the epoch just begun.
fn outranked(raw: &FaultyEngine, floor: u64) -> Option<String> {
    match rows(raw) {
        Ok(rows) => rows
            .iter()
            .find(|&(_, &seq)| seq >= floor)
            .map(|(key, seq)| format!("row {key:02x?} at {seq:#x} is not below {floor:#x}")),
        Err(e) => Some(e),
    }
}

impl CrashSubject for TxIndexSubject {
    type State = u64;
    type Files = NoFiles;

    fn prefix(&self) -> &'static str {
        "txindex-"
    }
    fn name(&self) -> &'static str {
        "txindex"
    }
    fn locator(&self) -> (Cf, &'static [u8]) {
        TXINDEX_ROWS.locator
    }
    fn db(&self) -> &'static DbSchema {
        &TXINDEX_DB
    }
    /// Inexact rows only: no journal bound.
    fn d_max(&self, _: u32) -> Option<u32> {
        None
    }
    fn data(
        &self,
        world: &World,
    ) -> Result<Box<dyn SubscriberData<E = FaultyEngine, State = u64>>, SubscriberError> {
        let engine = world.engine(SUB_STORE, &TXINDEX_DB);
        let data = TxIndexData::<FaultyEngine>::new(&*engine, Arc::new(SynthPositions))
            .map_err(|e| SubscriberError::Data(e.to_string()))?;
        if let Some(e) = world.checking(|| outranked(&engine, data.initial_state())) {
            panic!("txindex: a surviving row outranks the new epoch: {e}");
        }
        Ok(Box::new(data))
    }
    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::new(SynthBlocks)
    }
    /// Every transaction of every block up to L has its row, none of genesis's, and no row is
    /// from an epoch after the stored one.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        let (rows, epoch) = match (rows(raw), epoch(raw)) {
            (Ok(rows), Ok(epoch)) => (rows, epoch),
            (Err(e), _) | (_, Err(e)) => return Some(e),
        };
        if let Some((key, seq)) = rows.iter().find(|&(_, &seq)| seq >> SEQ_BITS > u64::from(epoch))
        {
            return Some(format!("row {key:02x?} at {seq:#x} is after epoch {epoch}"));
        }
        let genesis = path.first().map(|g| position(&g.hash));
        if rows.keys().any(|k| TxKey::decode(k).is_ok_and(|k| Some(k.pos.block) == genesis)) {
            return Some("a genesis transaction is indexed".into());
        }
        path.iter().find_map(|b| match keys(b) {
            Ok(keys) => keys
                .iter()
                .find(|k| !rows.contains_key(*k))
                .map(|k| format!("no row {k:02x?} for block {} at L", b.height)),
            Err(e) => Some(e),
        })
    }
    /// The driver reads every transaction's row up to V's tip.
    fn check_tip(&self, reader: &dyn CrashReader, path: &[BlockRef]) -> Option<String> {
        path.iter().find_map(|b| {
            let keys = match keys(b) {
                Ok(keys) => keys,
                Err(e) => return Some(e),
            };
            keys.iter().find_map(|k| match reader.read(TX, k) {
                Ok(Some(v)) if codec::decode_seq(&v).is_ok() => None,
                got => Some(format!("the read of {k:02x?} at {}: {got:?}", b.height)),
            })
        })
    }
}

fn scenario(name: &'static str, mode: SubMode, operations: &str) -> Scenario<ChainSut> {
    register_subject(&SUBJECT);
    Scenario::new(
        name,
        ChainConfig {
            index: Some(SubConfig::with_subject(&SUBJECT, mode, 96)),
            ..ChainConfig::default()
        },
        ops(operations),
    )
}

/// The forward trace's subscriber-store events, in order: (index, op).
fn sub_events(trace: &dashd_store::faulty::Trace) -> Vec<(u64, OpKind)> {
    trace
        .entries
        .iter()
        .filter_map(|e| match e {
            TraceEntry::Event {
                id,
                store,
                op,
                ..
            } if store == SUB_STORE => Some((id.index, *op)),
            _ => None,
        })
        .collect()
}

/// The exhaustive sweep: clean, every crash point covered with both kinds, including the
/// points just before and just after the first open's epoch write. Returns the forward trace.
fn sweep(sc: &Scenario<ChainSut>) -> dashd_store::faulty::Trace {
    assert_eq!(sc.window, CrashWindow::all());
    let (trace, violations) = forward_trace(sc);
    assert!(violations.is_empty(), "{}: forward: {violations:?}", sc.name);
    let sub = sub_events(&trace);
    let (epoch_write, _) = *sub.first().expect("subscriber events");
    assert_eq!(sub[0].1, OpKind::Append(CommitKind::Durable), "the epoch write comes first");
    assert!(
        sub[1..].iter().any(|(_, op)| *op == OpKind::Append(CommitKind::Buffered)),
        "{}: no rows were appended",
        sc.name
    );
    assert!(epoch_write > 0, "an event precedes the epoch write");
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
        let images: Vec<u64> =
            report.per_image.iter().filter(|(_, k, ..)| *k == kind).map(|(at, ..)| *at).collect();
        assert_eq!(images.len(), events, "{}: every crash point for {kind:?}", sc.name);
        for at in [epoch_write - 1, epoch_write] {
            assert!(images.contains(&at), "{}: no {kind:?} image at {at}", sc.name);
        }
    }
    trace
}

#[test]
fn txindex_synced_crash_sweep() {
    sweep(&scenario(
        "txindex-synced",
        SubMode::Synced {
            backlog: 0,
        },
        "c2;f;c2;r2:3;f",
    ));
}

/// A catch-up whose cadence commit lands behind progress (L < X < p): the commit's crash points,
/// before and after it, and the restart's re-delivery above X.
#[test]
fn txindex_syncing_crash_sweep_commits_behind_progress() {
    let trace = sweep(&scenario(
        "txindex-syncing",
        SubMode::Syncing {
            cadence: 8,
        },
        "c5;f;c5;s11;k;s2;r8:3;s20;f",
    ));
    let mut last_locator = None;
    let mut behind = false;
    for mark in trace.marks() {
        match mark {
            Mark::Locator {
                store,
                tip,
                kind,
            } if *store == SUB_STORE => last_locator = Some((*tip, *kind)),
            Mark::SubscriberState {
                store,
                synced: false,
                durable: Some(x),
                progress: Some(p),
                ..
            } if *store == SUB_STORE && x != p => {
                behind |= last_locator
                    .is_some_and(|(tip, kind)| tip == *x && kind != LocatorKind::Rewind);
            }
            _ => {}
        }
    }
    assert!(behind, "no catch-up commit behind progress");
}

/// c2-fw: the subject is registered, so its repro lines' `index=` forms parse back to it.
#[test]
fn txindex_index_forms_parse() {
    register_subject(&SUBJECT);
    for (form, mode) in [
        (
            "txindex-synced:0:96",
            SubMode::Synced {
                backlog: 0,
            },
        ),
        (
            "txindex-syncing:3:96",
            SubMode::Syncing {
                cadence: 3,
            },
        ),
    ] {
        let parsed: SubConfig = form.parse().expect(form);
        assert_eq!(parsed, SubConfig::with_subject(&SUBJECT, mode, 96), "{form}");
        assert_eq!(parsed.to_string(), form);
    }
}
